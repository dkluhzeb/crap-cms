//! Builder for [`AdminStartParams`].

use std::{path::PathBuf, sync::Arc};

use crate::{
    admin::server::AdminStartParams,
    config::CrapConfig,
    core::{JwtSecret, SharedPasswordProvider, rate_limit::LoginRateLimiter},
    service::AppInfra,
};

/// Builder for [`AdminStartParams`]. Created via [`AdminStartParams::builder`].
pub struct AdminStartParamsBuilder {
    config: Option<CrapConfig>,
    config_dir: Option<PathBuf>,
    jwt_secret: Option<JwtSecret>,
    login_limiter: Option<Arc<LoginRateLimiter>>,
    ip_login_limiter: Option<Arc<LoginRateLimiter>>,
    forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    ip_forgot_password_limiter: Option<Arc<LoginRateLimiter>>,
    mfa_limiter: Option<Arc<LoginRateLimiter>>,
    ip_mfa_limiter: Option<Arc<LoginRateLimiter>>,
    password_provider: Option<SharedPasswordProvider>,
    infra: Option<Arc<AppInfra>>,
}

impl AdminStartParamsBuilder {
    pub(crate) fn new() -> Self {
        Self {
            config: None,
            config_dir: None,
            jwt_secret: None,
            login_limiter: None,
            ip_login_limiter: None,
            forgot_password_limiter: None,
            ip_forgot_password_limiter: None,
            mfa_limiter: None,
            ip_mfa_limiter: None,
            password_provider: None,
            infra: None,
        }
    }

    pub fn config(mut self, config: CrapConfig) -> Self {
        self.config = Some(config);

        self
    }

    pub fn config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);

        self
    }

    pub fn jwt_secret(mut self, jwt_secret: impl Into<JwtSecret>) -> Self {
        self.jwt_secret = Some(jwt_secret.into());

        self
    }

    pub fn login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.login_limiter = Some(limiter);

        self
    }

    pub fn ip_login_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_login_limiter = Some(limiter);

        self
    }

    pub fn forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.forgot_password_limiter = Some(limiter);

        self
    }

    pub fn ip_forgot_password_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_forgot_password_limiter = Some(limiter);

        self
    }

    pub fn mfa_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.mfa_limiter = Some(limiter);

        self
    }

    pub fn ip_mfa_limiter(mut self, limiter: Arc<LoginRateLimiter>) -> Self {
        self.ip_mfa_limiter = Some(limiter);

        self
    }

    pub fn password_provider(mut self, password_provider: SharedPasswordProvider) -> Self {
        self.password_provider = Some(password_provider);

        self
    }

    /// Process-stable [`AppInfra`] assembled once at boot.
    pub fn infra(mut self, infra: Arc<AppInfra>) -> Self {
        self.infra = Some(infra);

        self
    }

    pub fn build(self) -> AdminStartParams {
        AdminStartParams {
            config: self.config.expect("config is required"),
            config_dir: self.config_dir.expect("config_dir is required"),
            jwt_secret: self.jwt_secret.expect("jwt_secret is required"),
            login_limiter: self.login_limiter.expect("login_limiter is required"),
            ip_login_limiter: self.ip_login_limiter.expect("ip_login_limiter is required"),
            forgot_password_limiter: self
                .forgot_password_limiter
                .expect("forgot_password_limiter is required"),
            ip_forgot_password_limiter: self
                .ip_forgot_password_limiter
                .expect("ip_forgot_password_limiter is required"),
            mfa_limiter: self.mfa_limiter.expect("mfa_limiter is required"),
            ip_mfa_limiter: self.ip_mfa_limiter.expect("ip_mfa_limiter is required"),
            password_provider: self
                .password_provider
                .expect("password_provider is required"),
            infra: self.infra.expect("infra is required"),
        }
    }
}
