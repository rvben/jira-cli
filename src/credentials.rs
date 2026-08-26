use crate::api::ApiError;

const SERVICE: &str = "jira-cli";

pub fn store(profile: &str, token: &str) -> Result<(), ApiError> {
    entry(profile)?
        .set_password(token)
        .map_err(|error| keyring_error("store", error))
}

pub fn load(profile: &str) -> Result<String, ApiError> {
    entry(profile)?
        .get_password()
        .map_err(|error| keyring_error("read", error))
}

pub fn load_optional(profile: &str) -> Result<Option<String>, ApiError> {
    match entry(profile)?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(keyring_error("read", error)),
    }
}

pub fn delete(profile: &str) -> Result<bool, ApiError> {
    match entry(profile)?.delete_credential() {
        Ok(()) => Ok(true),
        Err(keyring::Error::NoEntry) => Ok(false),
        Err(error) => Err(keyring_error("delete", error)),
    }
}

pub fn available() -> Result<(), ApiError> {
    keyring::Entry::store_status()
        .as_ref()
        .map_err(|error| unavailable_error(&error.to_string()))
        .map(|_| ())
}

fn entry(profile: &str) -> Result<keyring::Entry, ApiError> {
    keyring::Entry::new(SERVICE, &format!("profile:{profile}"))
        .map_err(|error| keyring_error("open", error))
}

fn unavailable_error(detail: &str) -> ApiError {
    if detail.contains("ServiceUnknown")
        && (detail.contains("org.freedesktop.secrets") || detail.contains("Secret Service"))
    {
        ApiError::InvalidInput(
            "OS credential store is unavailable: no Secret Service provider is running".into(),
        )
    } else {
        ApiError::InvalidInput(format!("OS credential store is unavailable: {detail}"))
    }
}

fn keyring_error(operation: &str, error: keyring::Error) -> ApiError {
    let message = match error {
        keyring::Error::NoEntry => {
            "credential not found for profile; run `jira auth login` or `jira auth migrate`"
                .to_string()
        }
        other => format!("failed to {operation} OS-keychain credential: {other}"),
    };
    ApiError::InvalidInput(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_linux_secret_service_has_a_human_error() {
        let error = unavailable_error(
            "Platform failure: org.freedesktop.DBus.Error.ServiceUnknown: the name org.freedesktop.secrets was not provided",
        );
        assert!(error.to_string().contains("no Secret Service provider"));
    }
}
