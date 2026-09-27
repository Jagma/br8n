use crate::backup::remote::{Remote, RemoteObject};
use crate::config::S3Config;
use anyhow::{anyhow, Context, Result};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::StorageClass;
use std::io::Write;
use std::path::Path;

pub struct S3Remote {
    rt: tokio::runtime::Runtime,
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    storage_class: StorageClass,
}

pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !text.ends_with(&cause_text) {
            text.push_str(": ");
            text.push_str(&cause_text);
        }
        source = cause.source();
    }
    text
}

pub fn normalized_prefix(prefix: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

impl S3Remote {
    pub fn new(cfg: &S3Config) -> Result<S3Remote> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("could not start a runtime for the S3 client")?;
        let conf = rt.block_on(
            aws_config::defaults(aws_config::BehaviorVersion::latest())
                .region(aws_config::Region::new(cfg.region.clone()))
                .profile_name(&cfg.profile)
                .load(),
        );
        Ok(S3Remote {
            rt,
            client: aws_sdk_s3::Client::new(&conf),
            bucket: cfg.bucket.clone(),
            prefix: normalized_prefix(&cfg.prefix),
            storage_class: StorageClass::from(cfg.storage_class.as_str()),
        })
    }

    pub fn full_key(&self, key: &str) -> String {
        format!("{}{key}", self.prefix)
    }

    pub fn relative_key<'a>(&self, full: &'a str) -> Option<&'a str> {
        full.strip_prefix(self.prefix.as_str())
    }

    pub fn check(&self) -> Result<()> {
        self.rt
            .block_on(self.client.head_bucket().bucket(&self.bucket).send())
            .map(|_| ())
            .map_err(|e| {
                anyhow!(
                    "could not reach s3://{} — check the bucket name, the region, and that the profile's credentials resolve from ~/.aws without environment variables: {}",
                    self.bucket,
                    error_chain(&e)
                )
            })
    }

    fn put_body(&self, key: &str, body: ByteStream) -> Result<()> {
        let full = self.full_key(key);
        self.rt
            .block_on(
                self.client
                    .put_object()
                    .bucket(&self.bucket)
                    .key(&full)
                    .storage_class(self.storage_class.clone())
                    .body(body)
                    .send(),
            )
            .map_err(|e| {
                anyhow!(
                    "could not upload s3://{}/{full}: {}",
                    self.bucket,
                    error_chain(&e)
                )
            })?;
        Ok(())
    }

    fn get_object(&self, key: &str) -> Result<aws_sdk_s3::operation::get_object::GetObjectOutput> {
        let full = self.full_key(key);
        self.rt
            .block_on(
                self.client
                    .get_object()
                    .bucket(&self.bucket)
                    .key(&full)
                    .send(),
            )
            .map_err(|e| {
                anyhow!(
                    "could not download s3://{}/{full}: {}",
                    self.bucket,
                    error_chain(&e)
                )
            })
    }
}

impl Remote for S3Remote {
    fn name(&self) -> &str {
        "s3"
    }

    fn put_file(&self, key: &str, path: &Path) -> Result<()> {
        let body = self
            .rt
            .block_on(ByteStream::from_path(path))
            .with_context(|| format!("could not read {}", path.display()))?;
        self.put_body(key, body)
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.put_body(key, ByteStream::from(bytes.to_vec()))
    }

    fn get_file(&self, key: &str, dest: &Path) -> Result<()> {
        let mut body = self.get_object(key)?.body;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(dest)
            .with_context(|| format!("could not create {}", dest.display()))?;
        self.rt.block_on(async {
            while let Some(chunk) = body
                .try_next()
                .await
                .with_context(|| format!("download of {key} was interrupted"))?
            {
                file.write_all(&chunk)?;
            }
            file.flush()?;
            Ok(())
        })
    }

    fn get_bytes(&self, key: &str) -> Result<Vec<u8>> {
        let body = self.get_object(key)?.body;
        let collected = self
            .rt
            .block_on(body.collect())
            .with_context(|| format!("download of {key} was interrupted"))?;
        Ok(collected.into_bytes().to_vec())
    }

    fn list(&self, prefix: &str) -> Result<Vec<RemoteObject>> {
        let full = self.full_key(prefix);
        self.rt.block_on(async {
            let mut out = Vec::new();
            let mut pages = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&full)
                .into_paginator()
                .send();
            while let Some(page) = pages.next().await {
                let page = page.map_err(|e| {
                    anyhow!(
                        "could not list s3://{}/{full}: {}",
                        self.bucket,
                        error_chain(&e)
                    )
                })?;
                for obj in page.contents() {
                    let Some(key) = obj.key().and_then(|k| self.relative_key(k)) else {
                        continue;
                    };
                    out.push(RemoteObject {
                        key: key.to_string(),
                        size: obj.size().unwrap_or(0).max(0) as u64,
                    });
                }
            }
            Ok(out)
        })
    }

    fn delete(&self, key: &str) -> Result<()> {
        let full = self.full_key(key);
        self.rt
            .block_on(
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(&full)
                    .send(),
            )
            .map_err(|e| {
                anyhow!(
                    "could not delete s3://{}/{full}: {}",
                    self.bucket,
                    error_chain(&e)
                )
            })?;
        Ok(())
    }
}
