use crate::backup::remote::{Remote, RemoteObject};
use crate::config::DriveConfig;
use anyhow::{anyhow, Context, Result};
use google_drive3::api::{File, Scope};
use google_drive3::{hyper, hyper_rustls, hyper_util, yup_oauth2, DriveHub};
use http_body_util::BodyExt;
use std::future::Future;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;

type Connector = hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>;

pub const FOLDER_NAME: &str = "br8n backups";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";

pub fn explain_auth_error(raw: &str) -> String {
    if raw.contains("invalid_grant") {
        return format!(
            "Google rejected the saved credentials ({raw}).\n\
             \n\
             This almost always means the OAuth app is still in \"Testing\" publishing status. \
             Google expires refresh tokens for apps in Testing after 7 days, so backups stop \
             about a week after each authorization.\n\
             \n\
             Fix it once: in the Google Cloud console, open APIs & Services > OAuth consent \
             screen and set the publishing status to \"In production\". Then re-run \
             `br8n backup auth drive`."
        );
    }
    raw.to_string()
}

pub fn quoted_query_literal(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

pub fn keys_under(
    names: impl IntoIterator<Item = (String, u64)>,
    prefix: &str,
) -> Vec<RemoteObject> {
    names
        .into_iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .map(|(key, size)| RemoteObject { key, size })
        .collect()
}

struct RefuseInteractiveConsent;

impl yup_oauth2::authenticator_delegate::InstalledFlowDelegate for RefuseInteractiveConsent {
    fn present_user_url<'a>(
        &'a self,
        _url: &'a str,
        _need_code: bool,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send + 'a>> {
        Box::pin(async {
            Err("the saved Google Drive token could not be refreshed, and an unattended run cannot ask for consent. Run `br8n backup auth drive` again, interactively.".to_string())
        })
    }
}

pub struct DriveRemote {
    rt: tokio::runtime::Runtime,
    hub: DriveHub<Connector>,
    folder_id: String,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("could not start a runtime for the Drive client")
}

fn tls_connector() -> Result<Connector> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Ok(hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .context("could not load the system's TLS root certificates")?
        .https_only()
        .enable_http1()
        .build())
}

fn client_with_body<B>(connector: Connector) -> hyper_util::client::legacy::Client<Connector, B>
where
    B: hyper::body::Body + Send,
    B::Data: Send,
{
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(connector)
}

async fn build_hub(
    secret_file: &Path,
    token_file: &Path,
    interactive: bool,
) -> Result<DriveHub<Connector>> {
    let secret = yup_oauth2::read_application_secret(secret_file)
        .await
        .with_context(|| {
            format!(
                "could not read the OAuth client secret at `{}`",
                secret_file.display()
            )
        })?;
    let method = if interactive {
        yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect
    } else {
        yup_oauth2::InstalledFlowReturnMethod::Interactive
    };
    let builder = yup_oauth2::InstalledFlowAuthenticator::with_client(
        secret,
        method,
        yup_oauth2::client::CustomHyperClientBuilder::from(client_with_body::<String>(
            tls_connector()?,
        )),
    )
    .persist_tokens_to_disk(token_file);
    let builder = if interactive {
        builder
    } else {
        builder.flow_delegate(Box::new(RefuseInteractiveConsent))
    };
    let auth = builder
        .build()
        .await
        .map_err(|e| anyhow!("{}", explain_auth_error(&e.to_string())))?;
    Ok(DriveHub::new(client_with_body(tls_connector()?), auth))
}

fn drive_error(what: &str, e: google_drive3::Error) -> anyhow::Error {
    anyhow!("{what}: {}", explain_auth_error(&e.to_string()))
}

impl DriveRemote {
    pub fn new(cfg: &DriveConfig, token_file: &Path) -> Result<DriveRemote> {
        if !token_file.exists() {
            return Err(anyhow!(
                "Google Drive is configured but not yet authorized (no token at `{}`).\n\
                 Run `br8n backup auth drive` once, interactively, then try again.",
                token_file.display()
            ));
        }
        if cfg.folder_id.is_empty() {
            return Err(anyhow!(
                "[backup.drive] has no `folder_id`. Run `br8n backup auth drive`; it creates the folder and prints the line to add."
            ));
        }
        let secret_file = crate::config::Config::expand_tilde_path(&cfg.client_secret_file);
        if !secret_file.exists() {
            return Err(anyhow!(
                "no OAuth client secret at `{}`. Download it from the Google Cloud console \
                 (APIs & Services > Credentials > OAuth client ID > Download JSON) and point \
                 `client_secret_file` at it.",
                secret_file.display()
            ));
        }
        let rt = runtime()?;
        let hub = rt.block_on(build_hub(&secret_file, token_file, false))?;
        Ok(DriveRemote {
            rt,
            hub,
            folder_id: cfg.folder_id.clone(),
        })
    }

    pub fn check(&self) -> Result<()> {
        self.rt
            .block_on(
                self.hub
                    .files()
                    .get(&self.folder_id)
                    .param("fields", "id,name")
                    .add_scope(Scope::File)
                    .doit(),
            )
            .map(|_| ())
            .map_err(|e| {
                drive_error(
                    &format!("could not open the Drive folder {}", self.folder_id),
                    e,
                )
            })
    }

    fn folder_query(&self) -> String {
        format!(
            "{} in parents and trashed = false",
            quoted_query_literal(&self.folder_id)
        )
    }

    fn files_named(&self, key: &str) -> Result<Vec<String>> {
        let q = format!(
            "name = {} and {}",
            quoted_query_literal(key),
            self.folder_query()
        );
        let (_, list) = self
            .rt
            .block_on(
                self.hub
                    .files()
                    .list()
                    .q(&q)
                    .param("fields", "files(id)")
                    .add_scope(Scope::File)
                    .doit(),
            )
            .map_err(|e| drive_error(&format!("could not look up `{key}` in Drive"), e))?;
        Ok(list
            .files
            .unwrap_or_default()
            .into_iter()
            .filter_map(|f| f.id)
            .collect())
    }

    fn only_file_named(&self, key: &str) -> Result<Option<String>> {
        let mut ids = self.files_named(key)?;
        match ids.len() {
            0 | 1 => Ok(ids.pop()),
            n => Err(anyhow!(
                "the Drive folder holds {n} files named `{key}`; remove the duplicates by hand so it is clear which one is current"
            )),
        }
    }

    fn upload(&self, key: &str, reader: impl google_drive3::common::ReadSeek) -> Result<()> {
        let mime: mime::Mime = mime::APPLICATION_OCTET_STREAM;
        match self.only_file_named(key)? {
            Some(id) => self
                .rt
                .block_on(
                    self.hub
                        .files()
                        .update(File::default(), &id)
                        .add_scope(Scope::File)
                        .upload_resumable(reader, mime),
                )
                .map_err(|e| drive_error(&format!("could not update `{key}` in Drive"), e))?,
            None => {
                let meta = File {
                    name: Some(key.to_string()),
                    parents: Some(vec![self.folder_id.clone()]),
                    ..Default::default()
                };
                self.rt
                    .block_on(
                        self.hub
                            .files()
                            .create(meta)
                            .add_scope(Scope::File)
                            .upload_resumable(reader, mime),
                    )
                    .map_err(|e| drive_error(&format!("could not upload `{key}` to Drive"), e))?
            }
        };
        Ok(())
    }

    fn download(&self, key: &str, mut sink: impl Write) -> Result<()> {
        let id = self
            .only_file_named(key)?
            .ok_or_else(|| anyhow!("no such object in Drive: {key}"))?;
        let (response, _) = self
            .rt
            .block_on(
                self.hub
                    .files()
                    .get(&id)
                    .param("alt", "media")
                    .add_scope(Scope::File)
                    .doit(),
            )
            .map_err(|e| drive_error(&format!("could not download `{key}` from Drive"), e))?;
        let mut body = response.into_body();
        self.rt.block_on(async {
            while let Some(frame) = body.frame().await {
                let frame =
                    frame.with_context(|| format!("download of `{key}` was interrupted"))?;
                if let Ok(data) = frame.into_data() {
                    sink.write_all(&data)?;
                }
            }
            sink.flush()?;
            Ok(())
        })
    }
}

pub fn authorize(client_secret_file: &Path, token_file: &Path, folder_id: &str) -> Result<String> {
    if let Some(parent) = token_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let rt = runtime()?;
    let folder_id = rt.block_on(async {
        let hub = build_hub(client_secret_file, token_file, true).await?;
        if !folder_id.is_empty() {
            hub.files()
                .get(folder_id)
                .param("fields", "id")
                .add_scope(Scope::File)
                .doit()
                .await
                .map_err(|e| drive_error(&format!("authorized, but folder {folder_id} is not visible to br8n (the drive.file scope only sees folders br8n created; clear `folder_id` and re-run to create one)"), e))?;
            return Ok::<String, anyhow::Error>(folder_id.to_string());
        }
        let meta = File {
            name: Some(FOLDER_NAME.to_string()),
            mime_type: Some(FOLDER_MIME.to_string()),
            ..Default::default()
        };
        let (_, created) = hub
            .files()
            .create(meta)
            .param("fields", "id")
            .add_scope(Scope::File)
            .upload(std::io::Cursor::new(Vec::new()), mime::APPLICATION_OCTET_STREAM)
            .await
            .map_err(|e| drive_error("could not create the backup folder in Drive", e))?;
        created
            .id
            .ok_or_else(|| anyhow!("Drive created the backup folder but returned no id"))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(token_file, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(folder_id)
}

impl Remote for DriveRemote {
    fn name(&self) -> &str {
        "drive"
    }

    fn put_file(&self, key: &str, path: &Path) -> Result<()> {
        let f = std::fs::File::open(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        self.upload(key, f)
    }

    fn put_bytes(&self, key: &str, bytes: &[u8]) -> Result<()> {
        self.upload(key, std::io::Cursor::new(bytes.to_vec()))
    }

    fn get_file(&self, key: &str, dest: &Path) -> Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::File::create(dest)
            .with_context(|| format!("could not create {}", dest.display()))?;
        self.download(key, std::io::BufWriter::new(file))
    }

    fn get_bytes(&self, key: &str) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.download(key, &mut out)?;
        Ok(out)
    }

    fn list(&self, prefix: &str) -> Result<Vec<RemoteObject>> {
        let q = self.folder_query();
        let mut named = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut call = self
                .hub
                .files()
                .list()
                .q(&q)
                .param("fields", "nextPageToken,files(name,size)")
                .page_size(1000)
                .add_scope(Scope::File);
            if let Some(t) = &page_token {
                call = call.page_token(t);
            }
            let (_, list) = self
                .rt
                .block_on(call.doit())
                .map_err(|e| drive_error("could not list the Drive folder", e))?;
            for f in list.files.unwrap_or_default() {
                if let Some(name) = f.name {
                    named.push((name, f.size.unwrap_or(0).max(0) as u64));
                }
            }
            page_token = list.next_page_token;
            if page_token.is_none() {
                break;
            }
        }
        Ok(keys_under(named, prefix))
    }

    fn delete(&self, key: &str) -> Result<()> {
        for id in self.files_named(key)? {
            self.rt
                .block_on(self.hub.files().delete(&id).add_scope(Scope::File).doit())
                .map_err(|e| drive_error(&format!("could not delete `{key}` from Drive"), e))?;
        }
        Ok(())
    }
}
