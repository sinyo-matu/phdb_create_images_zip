use aws_config::BehaviorVersion;
use aws_sdk_s3::{
    error::SdkError,
    operation::{get_object::GetObjectError, put_object::PutObjectError},
};
use lambda_runtime::{service_fn, Diagnostic, Error, LambdaEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::{Cursor, Write},
    time::Duration,
};
use thiserror::Error as ThisError;
use tokio::io::AsyncReadExt;
use zip::write::SimpleFileOptions;

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SizeTableRenderRequestBody {
    pub table_data: SizeTableRenderTableData,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SizeTableRenderTableData {
    pub title: String,
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OneLineRenderRequestBody {
    pub one_line_size_data: OneLineSizeData,
}

impl OneLineRenderRequestBody {
    pub fn new_from_size_zh(size_zh: String) -> Self {
        OneLineRenderRequestBody {
            one_line_size_data: OneLineSizeData {
                title: "关于尺码".to_string(),
                size: size_zh,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OneLineSizeData {
    pub title: String,
    pub size: String,
}

impl From<&SizeTable> for SizeTableRenderRequestBody {
    fn from(size_table: &SizeTable) -> Self {
        Self {
            table_data: SizeTableRenderTableData {
                title: "尺码表".to_string(),
                headers: size_table.head.clone(),
                rows: size_table.body.clone(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct Response {
    result: String,
    message: String,
}

const SIZE_TABLE_RENDER_URL: &str = "https://size-table-render.eliamo.workers.dev/image";
const SIZE_TABLE_RENDER_AUTH_TOKEN: &str = "kBvz7_EwBA2PpPXu8hP+xCygfDGr2vgo8yo44CMn";

struct SizeTableRenderClient {
    client: reqwest::Client,
    auth_token: String,
}

impl SizeTableRenderClient {
    pub fn new() -> Self {
        let client = reqwest::Client::new();
        let auth_token = SIZE_TABLE_RENDER_AUTH_TOKEN.to_string();
        Self { client, auth_token }
    }

    pub async fn render_size_table(&self, size_table: &SizeTable) -> Result<Vec<u8>, MyError> {
        let response = self
            .client
            .post(SIZE_TABLE_RENDER_URL)
            .query(&[("type", "size-table")])
            .bearer_auth(&self.auth_token)
            .json(&SizeTableRenderRequestBody::from(size_table))
            .send()
            .await?;
        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }

    pub async fn render_one_line_size(&self, size_zh: String) -> Result<Vec<u8>, MyError> {
        let response = self
            .client
            .post(SIZE_TABLE_RENDER_URL)
            .query(&[("type", "one-line-size")])
            .timeout(Duration::from_secs(
                std::env::var("HTTP_TIMEOUT")
                    .unwrap()
                    .parse::<u64>()
                    .unwrap(),
            ))
            .bearer_auth(&self.auth_token)
            .json(&OneLineRenderRequestBody::new_from_size_zh(size_zh))
            .send()
            .await?;
        let bytes = response.bytes().await?;
        Ok(bytes.to_vec())
    }
}

const SEPARATOR_PATTERN: &[char] = &['，', '、', ','];

#[tokio::main]
async fn main() -> Result<(), Error> {
    let func = service_fn(func);
    lambda_runtime::run(func).await?;
    Ok(())
}

#[derive(Debug, ThisError)]
enum MyError {
    #[error("S3: {0}")]
    S3GetObject(#[from] SdkError<GetObjectError>),
    #[error("S3: {0}")]
    S3PutObject(#[from] SdkError<PutObjectError>),
    #[error("Render: {0}")]
    Render(#[from] reqwest::Error),
    #[error("Zip: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("Io: {0}")]
    Io(#[from] std::io::Error),
    #[error("item_code not found in body")]
    ItemCodeNotSet,
    #[error("image_count not found in body")]
    ImageCountNotSet,
    #[error("failed to parse image count")]
    ImageCountParse,
    #[error("failed to parse item size")]
    ItemSizeParseError,
}

impl From<MyError> for Diagnostic {
    fn from(err: MyError) -> Self {
        match err {
            MyError::S3GetObject(err) => Diagnostic {
                error_type: "S3GetObject".into(),
                error_message: format!("{:?}", err),
            },
            MyError::S3PutObject(err) => Diagnostic {
                error_type: "S3PutObject".into(),
                error_message: format!("{:?}", err),
            },
            MyError::Render(err) => Diagnostic {
                error_type: "Render".into(),
                error_message: format!("{:?}", err),
            },
            MyError::Zip(err) => Diagnostic {
                error_type: "Zip".into(),
                error_message: format!("{:?}", err),
            },
            MyError::Io(err) => Diagnostic {
                error_type: "Io".into(),
                error_message: format!("{:?}", err),
            },
            MyError::ItemCodeNotSet => Diagnostic {
                error_type: "ItemCodeNotSet".into(),
                error_message: "item_code not found in body".into(),
            },
            MyError::ImageCountNotSet => Diagnostic {
                error_type: "ImageCountNotSet".into(),
                error_message: "image_count not found in body".into(),
            },
            MyError::ImageCountParse => Diagnostic {
                error_type: "ImageCountParse".into(),
                error_message: "failed to parse image count".into(),
            },
            MyError::ItemSizeParseError => Diagnostic {
                error_type: "ItemSizeParseError".into(),
                error_message: "failed to parse item size".into(),
            },
        }
    }
}

async fn func(event: LambdaEvent<Value>) -> Result<Value, MyError> {
    let render_client = SizeTableRenderClient::new();
    let config = aws_config::load_defaults(BehaviorVersion::latest()).await;
    let s3_client = aws_sdk_s3::Client::new(&config);
    let item_code = event
        .payload
        .get("item_code")
        .ok_or(MyError::ItemCodeNotSet)?
        .to_string()
        .trim_matches('"')
        .to_string();
    println!("item code is {}", &item_code);
    let image_count = event
        .payload
        .get("image_count")
        .ok_or(MyError::ImageCountNotSet)?
        .to_string()
        .trim_matches('"')
        .parse::<u32>()
        .map_err(|_| MyError::ImageCountParse)?;
    let body_option = event.payload.get("body");

    let mut image_bytes: Vec<Vec<u8>> = Vec::new();
    for no in 1..=image_count {
        let res = match s3_client
            .get_object()
            .bucket("phitemspics")
            .key(format!("{}_{}.jpeg", item_code, no))
            .send()
            .await
        {
            Ok(object) => object,
            Err(err) => {
                if let SdkError::ServiceError(service_error) = &err {
                    if let GetObjectError::NoSuchKey(_) = service_error.err() {
                        println!("no such key: {}_{}.jpeg", item_code, no);
                        continue;
                    }
                }
                return Err(MyError::S3GetObject(err));
            }
        };
        let res_body = res.body;
        let mut image_byte: Vec<u8> = Vec::new();
        res_body
            .into_async_read()
            .read_to_end(&mut image_byte)
            .await?;
        println!(
            "get image:{},len:{}",
            format_args!("{}_{}.jpeg", item_code, no),
            image_byte.len()
        );
        image_bytes.push(image_byte);
    }
    /////////////////////////////////////////////
    // if request not have body then this item not have a size data
    if body_option.is_none() {
        let mut zip_buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());
        let mut zip_writer = zip::ZipWriter::new(&mut zip_buf);
        let zip_options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        for (i, image_byte) in image_bytes.into_iter().enumerate() {
            zip_writer.start_file(format!("{}_{}.jpg", item_code, i + 1), zip_options)?;
            zip_writer.write_all(&image_byte)?;
        }
        zip_writer.finish()?;

        let zip_file_buf = zip_buf.into_inner();
        println!("read buf length:{}", zip_file_buf.len());
        s3_client
            .put_object()
            .bucket("phbundledimages")
            .key(format!("{}.zip", item_code))
            .body(zip_file_buf.into())
            .send()
            .await?;
        return Ok(json!(Response {
            result: "ok".to_string(),
            message: "".to_string()
        }));
    };
    ////////////////////////////////////////////////
    let item_size = serde_json::from_value::<ItemSize>(body_option.unwrap().to_owned())
        .map_err(|_| MyError::ItemSizeParseError)?;
    let size_image_bytes = match item_size.size_table {
        Some(mut size_table) => {
            let size_zh_escaped = item_size.size_zh.replace(SEPARATOR_PATTERN, " ");
            let table_head: Vec<String> = size_zh_escaped
                .trim()
                .split(' ')
                .map(|s| s.to_string())
                .collect();
            size_table.head = table_head;
            render_client.render_size_table(&size_table).await?
        }
        None => {
            let text = item_size.size_zh;
            let text = text.trim().replace(SEPARATOR_PATTERN, " ");
            render_client.render_one_line_size(text).await?
        }
    };

    let mut zip_buf: Cursor<Vec<u8>> = Cursor::new(Vec::new());
    let mut zip_writer = zip::ZipWriter::new(&mut zip_buf);
    let zip_options =
        SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (i, image_byte) in image_bytes.into_iter().enumerate() {
        zip_writer.start_file(format!("{}_{}.jpg", item_code, i + 1), zip_options)?;
        zip_writer.write_all(&image_byte)?;
    }
    zip_writer.start_file(format!("{}_size.jpg", item_code), zip_options)?;
    zip_writer.write_all(&size_image_bytes)?;
    zip_writer.finish()?;

    let zip_file_buf = zip_buf.into_inner();
    println!("read buf length:{}", zip_file_buf.len());
    s3_client
        .put_object()
        .bucket("phbundledimages")
        .body(zip_file_buf.into())
        .key(format!("{}.zip", item_code))
        .send()
        .await?;
    Ok(json!(Response {
        result: "ok".to_string(),
        message: "".to_string()
    }))
}
