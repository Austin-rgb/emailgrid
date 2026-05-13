use crate::{Brevo, EmailAddress, EmailingContext, Resend, Sender};
use ferrumec::di::{AsyncFromEnv, EnvContext, EnvError};
use sqlx::{Pool, Sqlite};
use std::sync::Arc;
impl AsyncFromEnv for OrphanWrapper<Arc<dyn Sender + Send + Sync>> {
    async fn from_env(ctx: &EnvContext) -> Result<Self, EnvError> {
        let sender_type = ctx.get("emailer.type")?;

        match sender_type {
            "resend" => Ok(OrphanWrapper(
                Arc::new(Resend(ctx.get("emailer.api")?.to_string()))
                    as Arc<dyn Sender + Send + Sync>,
            )),
            "brevo" => Ok(OrphanWrapper(
                Arc::new(Brevo(ctx.get("emailer.api")?.to_string()))
                    as Arc<dyn Sender + Send + Sync>,
            )),
            _ => Err(EnvError::new(format!(
                "Unsupported emailer.type value: {sender_type}"
            ))),
        }
    }
}

impl AsyncFromEnv for EmailingContext {
    async fn from_env(ctx: &EnvContext) -> Result<Self, EnvError> {
        let slf = EmailingContext::new(
            OrphanWrapper::<Arc<dyn Sender + Send + Sync>>::from_env(ctx)
                .await?
                .0,
            Pool::<Sqlite>::from_env(ctx).await?,
            EmailAddress::from_env(ctx).await?,
        )
        .map_err(|e| EnvError::new(e.to_string()))?;
        Ok(slf)
    }
}

impl AsyncFromEnv for EmailAddress {
    async fn from_env(ctx: &EnvContext) -> Result<Self, EnvError> {
        let email = ctx.get("email.address")?.to_string();
        let name = ctx.get("email.name")?.to_string();

        Ok(Self { email, name })
    }
}

pub struct OrphanWrapper<T>(pub T);
