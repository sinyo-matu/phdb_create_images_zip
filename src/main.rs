use image_combiner::{Processor, TableBase};
use lambda_runtime::{service_fn, Error, LambdaEvent};
use rusoto_core::{Region, RusotoError};
use rusoto_s3::{GetObjectError, GetObjectRequest, S3Client, S3};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use tokio::io::AsyncReadExt;

#[derive(Clone, Debug, Deserialize)]
struct ItemSize {
    size_table: Option<SizeTable>,
    #[allow(dead_code)]
    size_description: Option<String>,
    size_zh: String,
}

#[derive(Clone, Debug, Deserialize)]
struct SizeTable {
    #[allow(dead_code)]
    pub head: Vec<String>,
    pub body: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct Response {
    result: String,
    message: String,
}

const SEPARATOR_PATTERN: &[char] = &['，', '、', ','];

#[tokio::main]
async fn main() -> Result<(), Error> {
    let func = service_fn(func);
    lambda_runtime::run(func).await?;
    Ok(())
}

async fn func(event: LambdaEvent<Value>) -> Result<Value, Error> {
    let item_code = match event.payload.get("item_code") {
        Some(string) => string.as_str().unwrap().to_owned(),
        None => {
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "item_code not show".to_string()
            }))
        }
    };
    println!("item code is {}", &item_code);
    let image_count = match event.payload.get("image_count") {
        Some(image_count) => match image_count.to_string().parse::<u32>() {
            Ok(image_count_u32) => image_count_u32,
            Err(_) => {
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: "error when parse item count".to_string()
                }));
            }
        },
        None => {
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "image_count not show".to_string()
            }));
        }
    };
    let item_size_opt = match event.payload.get("body").unwrap().is_null() {
        true => None,
        false => {
            match serde_json::from_value::<ItemSize>(event.payload.get("body").unwrap().to_owned())
            {
                Ok(item_size) => Some(item_size),
                Err(err) => {
                    return Ok(json!(Response {
                        result: "error".to_string(),
                        message: format!("error when parse body field error: {:?}", err)
                    }));
                }
            }
        }
    };
    let s3_client = S3Client::new(Region::ApNortheast1);
    let mut image_bytes: Vec<Vec<u8>> = Vec::new();
    for no in 1..=image_count {
        let request = GetObjectRequest {
            bucket: "phitemspics".to_string(),
            key: format!("{}_{}.jpeg", item_code, no),
            ..Default::default()
        };
        let res = match s3_client.get_object(request).await {
            Ok(object) => object,
            Err(err) => {
                if let RusotoError::Service(GetObjectError::NoSuchKey(_)) = err {
                    println!("no such key:{}", format_args!("{}_{}.jpeg", item_code, no));
                    continue;
                }
                println!("error happened:{}", err);
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: "error when get item image".to_string()
                }));
            }
        };
        let res_body = res.body.unwrap();
        let mut image_byte: Vec<u8> = Vec::new();
        if let Err(err) = res_body
            .into_async_read()
            .read_to_end(&mut image_byte)
            .await
        {
            println!("error happened:{}", err);
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "error when read image bytes".to_string()
            }));
        }
        println!(
            "get image:{},len:{}",
            format_args!("{}_{}.jpeg", item_code, no),
            image_byte.len()
        );
        image_bytes.push(image_byte);
    }
    let zip_file_path = format!("/mnt/phdb/{}_images.zip", item_code);
    /////////////////////////////////////////////
    // if request not have body then this item not have a size data
    let processor = Processor::default();
    if item_size_opt.is_none() {
        let zip_file = match std::fs::File::create(&zip_file_path) {
            Ok(file) => file,
            Err(err) => {
                println!("error happened:{}", err);
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: "error when create zip file".to_string()
                }));
            }
        };

        let mut zip = zip::ZipWriter::new(&zip_file);
        let zip_options = zip::write::FileOptions::default();
        for (i, image_byte) in image_bytes.into_iter().enumerate() {
            if let Err(err) = zip.start_file(format!("{}_{}.jpg", item_code, i + 1), zip_options) {
                std::fs::remove_file(zip_file_path).unwrap();
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when zip start file error:{}", err)
                }));
            };

            if let Err(err) = zip.write_all(&image_byte) {
                std::fs::remove_file(zip_file_path).unwrap();
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when zip write file error:{}", err)
                }));
            };
        }
        if let Err(err) = zip.finish() {
            std::fs::remove_file(zip_file_path).unwrap();
            return Ok(json!(Response {
                result: "error".to_string(),
                message: format!("error when zip finish error:{}", err)
            }));
        }

        let file_meta = zip_file.metadata().unwrap();
        println!(
            "file:length:{},is file:{},file type:{:?}",
            file_meta.len(),
            file_meta.is_file(),
            file_meta.file_type()
        );
        let zip_file = match std::fs::File::open(&zip_file_path) {
            Ok(file) => file,
            Err(err) => {
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when open file error:{}", err)
                }));
            }
        };
        let mut zip_file_buf = Vec::new();
        let mut buf_reader = std::io::BufReader::new(zip_file);
        match buf_reader.read_to_end(&mut zip_file_buf) {
            Ok(_) => (),
            Err(err) => {
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when read zip file:{}", err)
                }));
            }
        }
        println!("read buf length:{}", zip_file_buf.len());
        let put_request = rusoto_s3::PutObjectRequest {
            bucket: "phbundledimages".to_string(),
            body: Some(zip_file_buf.into()),
            key: format!("{}.zip", item_code),
            ..Default::default()
        };
        if s3_client.put_object(put_request).await.is_err() {
            std::fs::remove_file(&zip_file_path).unwrap();
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "error when put image".to_string()
            }));
        }
        std::fs::remove_file(&zip_file_path).unwrap();
        return Ok(json!(Response {
            result: "ok".to_string(),
            message: "".to_string()
        }));
    };
    ////////////////////////////////////////////////
    let font_bytes = match get_font_file("TaipeiSansTCBeta-Light.ttf", &s3_client).await {
        Ok(font_byte) => font_byte,
        Err(err) => {
            println!("get font file error happened:{:?}", err);
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "can not found font file".to_string()
            }));
        }
    };
    let item_size = item_size_opt.unwrap();
    let size_image_bytes = match item_size.size_table {
        Some(size_table) => {
            let size_zh_escaped = item_size.size_zh.replace(SEPARATOR_PATTERN, " ");
            let table_head: Vec<String> = size_zh_escaped
                .trim()
                .split(' ')
                .map(|s| s.to_string())
                .collect();
            let table_base = match TableBase::new(table_head, size_table.body, 2) {
                Ok(table_base) => table_base,
                Err(err) => {
                    println!("error happened:{:?}", err);
                    return Ok(json!(Response {
                        result: "error".to_string(),
                        message: format!("error when create table base error: {:?}", err)
                    }));
                }
            };
            match processor.create_table_image(table_base, &font_bytes).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    println!("error happened:{:?}", err);
                    return Ok(json!(Response {
                        result: "error".to_string(),
                        message: format!("error when create table image error: {:?}", err)
                    }));
                }
            }
        }
        None => {
            let text = item_size.size_zh;
            let text = text.trim().replace(SEPARATOR_PATTERN, " ");
            match processor.create_text_image(&text, &font_bytes).await {
                Ok(bytes) => bytes,
                Err(err) => {
                    println!("error happened:{:?}", err);
                    return Ok(json!(Response {
                        result: "error".to_string(),
                        message: format!("error when create text image error: {:?}", err)
                    }));
                }
            }
        }
    };
    let zip_file = match std::fs::File::create(&zip_file_path) {
        Ok(file) => file,
        Err(err) => {
            println!("error happened:{}", err);
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "error when create zip file".to_string()
            }));
        }
    };
    {
        let mut zip = zip::ZipWriter::new(&zip_file);
        let zip_options = zip::write::FileOptions::default();
        for (i, image_byte) in image_bytes.into_iter().enumerate() {
            if let Err(err) = zip.start_file(format!("{}_{}.jpg", item_code, i + 1), zip_options) {
                std::fs::remove_file(&zip_file_path).unwrap();
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when zip start file error:{}", err)
                }));
            };

            if let Err(err) = zip.write_all(&image_byte) {
                std::fs::remove_file(&zip_file_path).unwrap();
                return Ok(json!(Response {
                    result: "error".to_string(),
                    message: format!("error when zip write file error:{}", err)
                }));
            };
        }
        if let Err(err) = zip.start_file(format!("{}_size.jpg", item_code), zip_options) {
            std::fs::remove_file(&zip_file_path).unwrap();
            return Ok(json!(Response {
                result: "error".to_string(),
                message: format!("error when zip start file error:{}", err)
            }));
        }
        if let Err(err) = zip.write_all(&size_image_bytes) {
            std::fs::remove_file(&zip_file_path).unwrap();
            return Ok(json!(Response {
                result: "error".to_string(),
                message: format!("error when zip write file error:{}", err)
            }));
        };

        if let Err(err) = zip.finish() {
            std::fs::remove_file(zip_file_path).unwrap();
            return Ok(json!(Response {
                result: "error".to_string(),
                message: format!("error when zip finish error:{}", err)
            }));
        }
    }

    let zip_file = match std::fs::File::open(&zip_file_path) {
        Ok(file) => file,
        Err(err) => {
            println!("error happened:{}", err);
            return Ok(json!(Response {
                result: "error".to_string(),
                message: "error when create zip file".to_string()
            }));
        }
    };
    let mut zip_file_buf = Vec::new();
    let mut reader = std::io::BufReader::new(zip_file);
    if let Err(err) = reader.read_to_end(&mut zip_file_buf) {
        std::fs::remove_file(zip_file_path).unwrap();
        return Ok(json!(Response {
            result: "error".to_string(),
            message: format!("error when reading zip file :{}", err)
        }));
    }

    let put_request = rusoto_s3::PutObjectRequest {
        bucket: "phbundledimages".to_string(),
        body: Some(zip_file_buf.into()),
        key: format!("{}.zip", item_code),
        ..Default::default()
    };
    if let Err(err) = s3_client.put_object(put_request).await {
        std::fs::remove_file(zip_file_path).unwrap();
        println!("error happened:{:?}", err);
        return Ok(json!(Response {
            result: "error".to_string(),
            message: format!("put file error: {:?}", err)
        }));
    }
    std::fs::remove_file(zip_file_path).unwrap();
    Ok(json!(Response {
        result: "ok".to_string(),
        message: "".to_string()
    }))
}

async fn get_font_file(key: &str, s3_client: &S3Client) -> Result<Vec<u8>, Error> {
    let request = GetObjectRequest {
        bucket: "phfunctionresource".into(),
        key: key.into(),
        ..Default::default()
    };
    let res = s3_client.get_object(request).await?;
    let res_body = res.body.unwrap();
    let mut font_bytes: Vec<u8> = Vec::new();
    res_body
        .into_async_read()
        .read_to_end(&mut font_bytes)
        .await?;
    Ok(font_bytes)
}
