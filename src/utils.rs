use std::sync::Arc;

use anyhow::{Result, anyhow};
use moka::future::Cache;
use serde_json::Value;
use sqlx::{Pool, Sqlite};
use tera::{Context, Tera};

use crate::emailing::{Attachment, EmailAddress, EmailPayload, SendResult, Sender};

/// Emails store with DB + cache
#[derive(Clone)]
pub struct Emails {
    pool: Pool<Sqlite>,
    cache: Cache<String, String>, // user_id -> email
}

impl Emails {
    /// Construct store
    pub fn new(pool: Pool<Sqlite>) -> Self {
        let cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(std::time::Duration::from_secs(60 * 10))
            .build();

        Self { pool, cache }
    }

    /// Add email for a user
    pub async fn add(&self, user_id: String, email: String) -> Result<()> {
        // Persist
        sqlx::query(
            r#"
            INSERT INTO emails (user_id, email)
            VALUES (?, ?)
            ON CONFLICT(user_id) DO UPDATE SET email = excluded.email
            "#,
        )
        .bind(&user_id)
        .bind(&email)
        .execute(&self.pool)
        .await?;

        // Update cache
        self.cache.insert(user_id, email).await;

        Ok(())
    }

    /// Get email for a user (cache-first)
    pub async fn get(&self, user_id: String) -> Result<String> {
        // Try cache
        if let Some(email) = self.cache.get(&user_id).await {
            return Ok(email);
        }

        // Fallback to DB
        let record = sqlx::query_scalar::<_, String>("SELECT email FROM emails WHERE user_id = ?")
            .bind(&user_id)
            .fetch_optional(&self.pool)
            .await?;

        match record {
            Some(email) => {
                self.cache.insert(user_id.clone(), email.clone()).await;
                Ok(email)
            }
            None => Err(anyhow!("Email not found for user_id")),
        }
    }
}

/// Builder for email content
#[derive(Clone)]
pub struct Builder {
    emails: Emails,
    tera: Arc<Tera>,
}

impl Builder {
    /// Construct builder
    pub fn new(emails: Emails) -> Result<Self> {
        // Load templates from ./templates directory
        let tera = Tera::new("templates/**/*")?;

        Ok(Self {
            emails,
            tera: Arc::new(tera),
        })
    }

    /// Build email
    pub async fn build(&self, subject: String, message: String) -> Result<(String, String)> {
        // Parse JSON message
        let data: Value = serde_json::from_str(&message)?;

        // Extract user_id
        let user_id = data
            .get("user_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("user_id missing in message"))?
            .to_string();

        // Get email
        let email = self.emails.get(user_id).await?;

        // Prepare template name
        let template_name = format!("{}.html", subject);

        // Build Tera context
        let mut context = Context::new();
        context.extend(Context::from_serialize(&data)?);

        // Render template
        let rendered = self.tera.render(&template_name, &context)?;

        Ok((email, rendered))
    }
}

#[derive(Clone)]
pub struct EmailingContext {
    sender: Arc<dyn Sender>,
    builder: Builder,
    default_sender: EmailAddress,
    default_attachments: Vec<Attachment>,
}

impl EmailingContext {
    pub fn new(
        sender: Arc<dyn Sender>,
        pool: Pool<Sqlite>,
        default_sender: EmailAddress,
    ) -> Result<Self> {
        let emails = Emails::new(pool);
        let builder = Builder::new(emails)?;

       Ok( Self {
            sender,
            builder,
            default_sender,
            default_attachments: Vec::new(),
        })
    }

    /// Optional: allow configuring default attachments
    pub fn with_attachments(mut self, attachments: Vec<Attachment>) -> Self {
        self.default_attachments = attachments;
        self
    }

    /// High-level API: does everything
    pub async fn send(&self, subject: String, message: String) -> Result<SendResult> {
        // 1. Build (resolve email + render template)
        let (email, html) = self.builder.build(subject.clone(), message).await?;

        // 2. Construct payload
        let payload = EmailPayload {
            sender: self.default_sender.clone(),
            to: vec![EmailAddress {
                email,
                name: "".to_string(), // You might want to set this properly
            }],
            subject,
            htmlContent: html,
            attachments: self.default_attachments.clone(),
        };

        // 3. Send
        let result = self.sender.send(&payload).await;

        Ok(result)
    }
}
