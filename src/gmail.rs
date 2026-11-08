use super::emailing::{EmailAddress, EmailPayload, SendResult, Sender};
use async_trait::async_trait;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{
        Attachment as LettreAttachment, Mailbox, MultiPart, SinglePart, header::ContentType,
    },
    transport::smtp::authentication::Credentials,
};
use std::{env, str::FromStr};

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

pub struct Gmail {
    mailer: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}
impl Gmail {
    pub fn new() -> anyhow::Result<Self> {
        let user = env::var("GMAIL_USER")?;
        let pass = env::var("GMAIL_APP_PASSWORD")?.replace(' ', "");

        let creds = Credentials::new(user.clone(), pass);

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay("smtp.gmail.com")?
            .credentials(creds)
            .build();

        let from = Mailbox::from_str(&user)?;

        Ok(Gmail { mailer, from })
    }

    /// `path` may be an http(s) URL or a local file path.
    async fn load_attachment(&self, path: &str) -> Result<Vec<u8>, BoxErr> {
        if path.starts_with("http://") || path.starts_with("https://") {
            let res = reqwest::get(path).await?.error_for_status()?;
            Ok(res.bytes().await?.to_vec())
        } else {
            Ok(tokio::fs::read(path).await?)
        }
    }

    async fn build_message(&self, p: &EmailPayload) -> Result<Message, BoxErr> {
        // Enforce a single From: the SMTP login. Gmail rejects spoofed senders.
        let _ = &p.sender; // caller may have provided one; we deliberately ignore it.

        let to: Vec<Mailbox> = p.to.iter().map(mailbox_from).collect::<Result<_, _>>()?;

        // Body part
        let body = SinglePart::builder()
            .header(ContentType::parse("text/html; charset=utf-8")?)
            .body(p.html_content.clone());

        // Attachments
        let mut parts: Vec<SinglePart> = Vec::with_capacity(p.attachments.len());
        for a in &p.attachments {
            let data = self.load_attachment(&a.path).await?;
            let att = LettreAttachment::new(a.filename.clone())
                .body(data, guess_content_type(&a.filename));
            parts.push(att);
        }

        let mut builder = Message::builder()
            .from(self.from.clone())
            .subject(&p.subject);

        for addr in &to {
            builder = builder.to(addr.clone());
        }

        let message = if parts.is_empty() {
            builder.multipart(MultiPart::alternative().singlepart(body))?
        } else {
            // mixed(multipart/alternative(html), attachments...)
            let alternative = MultiPart::alternative().singlepart(body);
            let mut mixed = MultiPart::mixed().multipart(alternative);
            for part in parts {
                mixed = mixed.singlepart(part);
            }
            builder.multipart(mixed)?
        };

        Ok(message)
    }
}

fn mailbox_from(a: &EmailAddress) -> Result<Mailbox, BoxErr> {
    if a.name.is_empty() {
        Ok(Mailbox::from_str(&a.email)?)
    } else {
        Ok(Mailbox::new(Some(a.name.clone()), a.email.parse()?))
    }
}

fn guess_content_type(filename: &str) -> ContentType {
    let ext = filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "txt" | "csv" => "text/plain",
        "html" | "htm" => "text/html",
        "zip" => "application/zip",
        "json" => "application/json",
        _ => "application/octet-stream",
    };
    ContentType::parse(mime).expect("static mime")
}

#[async_trait]
impl Sender for Gmail {
    async fn send(&self, payload: &EmailPayload) -> SendResult {
        let message = self.build_message(payload).await?;

        // `lettre` returns its own error type, but `SendResult` is
        // `Result<Response, _>` — so we surface success/failure as a
        // synthesized `Response`. Callers only inspect status.
        match self.mailer.send(message).await {
            Ok(resp) => {
                let status = reqwest::StatusCode::from_u16(200).unwrap();
                let _ = resp; // SMTP response (message id etc.)
                Ok(build_synthetic_response(
                    status,
                    "email sent via gmail smtp",
                ))
            }
            Err(e) => {
                tracing::error!("sending email failed: {e}");
                let status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
                Ok(build_synthetic_response(
                    status,
                    &format!("gmail smtp error: {e}"),
                ))
            }
        }
    }
}

fn build_synthetic_response(status: reqwest::StatusCode, body: &str) -> reqwest::Response {
    // Minimal `http::Response<Body>` → `reqwest::Response` conversion.
    let http_resp = http::Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(reqwest::Body::from(body.to_string()))
        .expect("valid response");
    reqwest::Response::from(http_resp)
}
