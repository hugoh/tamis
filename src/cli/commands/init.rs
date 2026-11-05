use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;

use anyhow::{Context as _, Result, bail};

use super::super::App;

pub(in super::super) fn init(app: &mut App, host: &str, user: &str) -> Result<()> {
    let path = app.config_path.clone();
    let text = format!(
        "[account]\nhost = {}\nuser = {}\n",
        toml_string(host),
        toml_string(user)
    );

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir).context("create config directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Owner-only, because password_command in the config runs what it names.
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .context("create config directory")?;
        }
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!("config already exists; edit it instead: {}", path.display())
        }
        Err(err) => return Err(err).context("create config"),
    };
    file.write_all(text.as_bytes()).context("write config")?;

    writeln!(
        app.env.stdout,
        "Wrote {}.\nNext: tamis login, to store the password in the system keychain.",
        path.display()
    )?;
    Ok(())
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            c if c.is_control() => {
                let _ = write!(quoted, "\\u{:04X}", u32::from(c));
            }
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}
