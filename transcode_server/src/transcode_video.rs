use crate::encrypt_file::encrypt_file_xchacha20;
use crate::encrypted_cid::create_encrypted_cid;
use crate::s5::hash_blake3_file;
use crate::s5::upload_video;
use crate::utils::{
    base64url_to_bytes, bytes_to_base64url, download_and_concat_files, download_video,
    hash_bytes_to_cid,
};
use base64::{engine::general_purpose, DecodeError, Engine as _};
use dotenv::var;
use sanitize_filename::sanitize;
use serde::Deserialize;
use serde_json;
use std::error::Error;
use std::fs::metadata;
use std::path::Path;
use std::process::Command;
use tokio::io::AsyncReadExt;
use tonic::{transport::Server, Code, Request, Response, Status};
use transcode::TranscodeResponse;

static PATH_TO_FILE: &str = "path/to/file/";

pub mod transcode {
    tonic::include_proto!("transcode");
}

#[derive(Debug, Deserialize)]
struct VideoFormat {
    id: u32,
    ext: String,
    vcodec: Option<String>,
    acodec: Option<String>,
    preset: Option<String>,
    profile: Option<String>,
    ch: Option<u8>,
    vf: Option<String>,
    b_v: Option<String>,
    ar: Option<String>,
    minrate: Option<String>,
    maxrate: Option<String>,
    bufsize: Option<String>,
    gpu: Option<bool>,
    compression_level: Option<u8>,
}

fn add_arg(cmd: &mut Command, arg: &str, value: Option<&str>) {
    if let Some(value) = value {
        cmd.arg(arg).arg(value);
    }
}

/// Transcodes the video at the specified `input_path` using ffmpeg
/// and saves the resulting output to the specified `output_path`.
/// Returns the path to the transcoded video as a `String`.
///
/// # Arguments
///
/// * `input_path` - The path to the input video file.
/// * `output_path` - The path to save the transcoded video file.
/// * `transcoder` - The transcoder to use for transcoding the video.
///
pub async fn transcode_video(
    file_path: &str,
    video_format: &str,
    is_encrypted: bool,
    is_gpu: bool,
) -> Result<Response<TranscodeResponse>, Status> {
    println!("transcode_video: Processing video at: {}", file_path);
    println!("transcode_video: video_format: {}", video_format);
    println!("transcode_video: is_encrypted: {}", is_encrypted);
    println!("transcode_video: is_gpu: {}", is_gpu);

    let unsanitized_file_name = Path::new(file_path)
        .file_name()
        .ok_or_else(|| Status::new(Code::InvalidArgument, "Invalid file path"))?
        .to_string_lossy()
        .to_string();

    let file_name = sanitize(&unsanitized_file_name);

    println!("Transcoding video: {}", &file_path);
    println!("is_gpu = {}", &is_gpu);

    let mut encryption_key1: Vec<u8> = Vec::new();

    let mut response: TranscodeResponse;

    let format: VideoFormat = serde_json::from_str::<VideoFormat>(video_format).map_err(|err| {
        Status::new(
            Code::InvalidArgument,
            format!("Invalid video format: {}", err),
        )
    })?;

    let mut cmd = Command::new("ffmpeg");

    if is_gpu {
        println!("GPU transcoding");

        add_arg(&mut cmd, "-i", Some(file_path));
        add_arg(&mut cmd, "-c:v", format.vcodec.as_deref());
        add_arg(&mut cmd, "-b:v", format.b_v.as_deref());
        add_arg(&mut cmd, "-c:a", Some("libopus")); // Keep this as-is, if not present in VideoFormat
        add_arg(&mut cmd, "-b:a", Some("192k")); // Keep this as-is, if not present in VideoFormat
        if let Some(ch) = format.ch {
            add_arg(&mut cmd, "-ac", Some(&ch.to_string()));
        }
        add_arg(&mut cmd, "-ar", format.ar.as_deref());
        add_arg(&mut cmd, "-vf", format.vf.as_deref());
        if let Some(ref minrate) = format.minrate {
            cmd.args(["-minrate", minrate]);
        }

        if let Some(ref maxrate) = format.maxrate {
            cmd.args(["-maxrate", maxrate]);
        }

        if let Some(ref bufsize) = format.bufsize {
            cmd.args(["-bufsize", bufsize]);
        }

        cmd.args([
            "-y",
            format!("./temp/to/transcode/{}_ue.{}", file_name, format.ext).as_str(),
        ]);
    } else {
        println!("CPU transcoding");

        if let Some(vcodec) = &format.vcodec {
            if !vcodec.is_empty() {
                add_arg(&mut cmd, "-i", Some(file_path));
                add_arg(&mut cmd, "-c:v", format.vcodec.as_deref());
                add_arg(&mut cmd, "-cpu-used", Some("4")); // set encoding speed to 4 (range 0-8, lower is slower)
                add_arg(&mut cmd, "-b:v", format.b_v.as_deref());
                add_arg(&mut cmd, "-crf", Some("30")); // set quality level to 30 (range 0-63, lower is better)
                add_arg(&mut cmd, "-c:a", Some("libopus")); // use libopus encoder for audio
                add_arg(&mut cmd, "-b:a", Some("192k")); // Keep this as-is, if not present in VideoFormat
                if let Some(ch) = format.ch {
                    add_arg(&mut cmd, "-ac", Some(&ch.to_string()));
                }
                add_arg(&mut cmd, "-vf", format.vf.as_deref());
                add_arg(
                    &mut cmd,
                    "-y",
                    Some(&format!(
                        "./temp/to/transcode/{}_ue.{}",
                        file_name, format.ext
                    )),
                );
            } else {
                return Err(Status::new(
                    Code::InvalidArgument,
                    "No video codec specified",
                ));
            }
        } else if let Some(acodec) = &format.acodec {
            if !acodec.is_empty() {
                println!("Transcoding audio");
                add_arg(&mut cmd, "-i", Some(file_path));
                add_arg(&mut cmd, "-acodec", format.acodec.as_deref());
                if let Some(ch) = format.ch {
                    add_arg(&mut cmd, "-ac", Some(&ch.to_string()));
                }
                add_arg(&mut cmd, "-ar", format.ar.as_deref());

                if let Some(compression_level) = format.compression_level {
                    add_arg(
                        &mut cmd,
                        "-compression_level",
                        Some(&compression_level.to_string()),
                    );
                }
                add_arg(
                    &mut cmd,
                    "-y",
                    Some(&format!(
                        "./temp/to/transcode/{}_ue.{}",
                        file_name, format.ext
                    )),
                );
            } else {
                return Err(Status::new(
                    Code::InvalidArgument,
                    "No audio codec specified",
                ));
            }
        } else {
            return Err(Status::new(Code::InvalidArgument, "No codec specified"));
        }
    }

    println!("Transcode cmd {:?}", cmd);

    let output = cmd.output().expect("Failed to execute command");
    println!("Transcode output {:?}", output);

    if is_encrypted {
        match encrypt_file_xchacha20(
            format!("./temp/to/transcode/{}_ue.{}", file_name, format.ext),
            format!("./temp/to/transcode/{}.{}", file_name, format.ext),
            0,
        ) {
            Ok(bytes) => {
                // Encryption succeeded, and `bytes` contains the encrypted data
                // Add your success handling code here
                encryption_key1 = bytes;
                println!("Encryption succeeded");
            }
            Err(error) => {
                // Encryption failed
                // Handle the error here
                eprintln!("Encryption error: {:?}", error);
                // Optionally, you can return an error or perform error-specific handling
            }
        }

        let file_path = format!("./temp/to/transcode/{}_ue.{}", file_name, format.ext);
        let file_path_encrypted = format!("./temp/to/transcode/{}.{}", file_name, format.ext);

        let hash_result = hash_blake3_file(file_path.clone());
        let hash_result_encrypted = hash_blake3_file(file_path_encrypted.to_owned());

        let cid_type_encrypted: u8 = 0xae; // replace with your actual cid type encrypted
        let encryption_algorithm: u8 = 0xa6; // replace with your actual encryption algorithm
        let chunk_size_as_power_of_2: u8 = 18; // replace with your actual chunk size as power of 2
        let padding: u32 = 0; // replace with your actual padding

        // Upload the transcoded videos to storage
        match upload_video(file_path_encrypted.as_str()) {
            Ok(cid_encrypted) => {
                println!(
                    "****************************************** cid: {:?}",
                    &cid_encrypted
                );

                let mut hash = Vec::new();
                match hash_result {
                    Ok(hash1) => {
                        hash = hash1.as_bytes().to_vec();
                        // Now you can use bytes as needed.
                    }
                    Err(err) => {
                        eprintln!("Error computing blake3 hash: {}", err);

                        return Err(Status::new(
                            Code::Internal,
                            format!("Error computing blake3 hash: {}", err),
                        ));
                    }
                }

                let mut hash_encrypted = Vec::new();
                match hash_result_encrypted {
                    Ok(hash1) => {
                        hash_encrypted = hash1.as_bytes().to_vec();
                        // Now you can use bytes as needed.
                    }
                    Err(err) => {
                        eprintln!("Error computing blake3 hash: {}", err);

                        return Err(Status::new(
                            Code::Internal,
                            format!("Error computing blake3 hash: {}", err),
                        ));
                    }
                }

                let mut encrypted_blob_hash = vec![0x1f];
                encrypted_blob_hash.extend(hash_encrypted);

                let cloned_hash = encrypted_blob_hash.clone();

                let file_path_path = Path::new(&file_path);
                let metadata = std::fs::metadata(file_path_path).expect("Failed to read metadata");
                let file_size = metadata.len();

                let cid = hash_bytes_to_cid(hash, file_size);

                println!("encryption_key1: {:?}", encryption_key1);
                println!("cid_encrypted: {:?}", cid_encrypted);
                println!("cid: {:?}", cid);

                println!(
                    "upload_video Ok: encrypted_blob_hash = {:?}",
                    hex::encode(&encrypted_blob_hash)
                );
                println!(
                    "upload_video Ok: encryption_key1 = {:?}",
                    hex::encode(&encryption_key1)
                );
                println!("upload_video Ok: cid = {:?}", hex::encode(&cid));

                let hash = hash_blake3_file(file_path_encrypted).unwrap();
                println!(
                    "`upload_video: encryptedBlobMHashBase64url` = {}",
                    general_purpose::URL_SAFE_NO_PAD
                        .encode([&[31u8] as &[_], hash.as_bytes()].concat())
                );

                let encrypted_cid_bytes = create_encrypted_cid(
                    cid_type_encrypted,
                    encryption_algorithm,
                    chunk_size_as_power_of_2,
                    encrypted_blob_hash,
                    encryption_key1,
                    padding,
                    cid,
                );

                println!(
                    "upload_video Ok: encrypted_cid_bytes = {:?}",
                    hex::encode(&encrypted_cid_bytes)
                );
                let encrypted_cid = format!("u{}", bytes_to_base64url(&encrypted_cid_bytes));
                println!("upload_video Ok: encrypted_cid = {}", encrypted_cid);

                // Now you have your encrypted_blob_hash and encrypted_cid
                println!("Encrypted Blob Hash: {:02x?}", cloned_hash);
                println!("Encrypted CID: {:?}", encrypted_cid);

                println!("Transcoding task finished");

                // Return the TranscodeResponse with the job ID
                response = TranscodeResponse {
                    status_code: 200,
                    message: String::from("Transcoding successful"),
                    cid: encrypted_cid,
                };
            }
            Err(e) => {
                println!("!!!!!!!!!!!!!!!!!!!!!2160p no cid");
                println!("Error: {}", e); // This line is added to print out the error message

                response = TranscodeResponse {
                    status_code: 500,
                    message: format!("Transcoding task failed with error {}", e),
                    cid: "".to_string(),
                };
            }
        };
    } else {
        let file_path = format!("./temp/to/transcode/{}_ue.{}", file_name, format.ext);

        // Upload the transcoded videos to storage
        match upload_video(file_path.as_str()) {
            Ok(cid_bytes) => {
                let cid = format!("u{}", bytes_to_base64url(&cid_bytes));
                println!("cid: {:?}", cid);

                println!("Transcoding task finished");

                // Return the TranscodeResponse with the job ID
                response = TranscodeResponse {
                    status_code: 200,
                    message: String::from("Transcoding successful"),
                    cid,
                };
            }
            Err(e) => {
                println!("!!!!!!!!!!!!!!!!!!!!!2160p no cid");
                println!("Error: {}", e); // This line is added to print out the error message

                response = TranscodeResponse {
                    status_code: 500,
                    message: format!("Transcoding task failed with error {}", e),
                    cid: "".to_string(),
                };
            }
        };
    }

    Ok(Response::new(response))
}
