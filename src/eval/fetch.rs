//! Copies the latest messages of some folders to disk, to evaluate on.

use std::path::Path;

use super::Error;
use crate::mail::Mailbox;

const BATCH: usize = 50;

/// `Folder=N`: the latest N messages of a folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spec {
    pub folder: String,
    pub limit: usize,
}

/// # Errors
/// When the text isn't `Folder=N` with N above zero.
pub fn parse_spec(text: &str) -> Result<Spec, String> {
    let wrong = || format!("{text:?}: want Folder=N, with N above zero");
    let (folder, limit) = text
        .rsplit_once('=')
        .filter(|(folder, _)| !folder.is_empty())
        .ok_or_else(wrong)?;
    let limit = limit
        .parse::<usize>()
        .ok()
        .filter(|limit| *limit > 0)
        .ok_or_else(wrong)?;
    Ok(Spec {
        folder: folder.to_owned(),
        limit,
    })
}

/// Writes the latest `limit` messages of `folder` into `dir` as `UID.eml`,
/// readable only by the owner, and returns how many.
///
/// # Errors
/// When the folder can't be read or a file can't be written.
pub async fn fetch_folder(
    client: &mut impl Mailbox,
    spec: &Spec,
    dir: &Path,
) -> Result<usize, Error> {
    let mut uids = client
        .uids_after(&spec.folder, 0)
        .await
        .map_err(|err| Error::Io(format!("list {}: {err}", spec.folder)))?;
    let skip = uids.len().saturating_sub(spec.limit);
    uids.drain(..skip);

    create_private_dir(dir)?;
    let mut written = 0;
    for batch in uids.chunks(BATCH) {
        let messages = client
            .fetch(&spec.folder, batch)
            .await
            .map_err(|err| Error::Io(format!("fetch from {}: {err}", spec.folder)))?;
        for message in messages {
            let name = dir.join(format!("{:08}.eml", message.uid));
            write_private(&name, &message.raw)?;
            written += 1;
        }
    }
    Ok(written)
}

fn create_private_dir(dir: &Path) -> Result<(), Error> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
        .create(dir)
        .map_err(|err| Error::Io(format!("create {}: {err}", dir.display())))
}

fn write_private(path: &Path, content: &[u8]) -> Result<(), Error> {
    use std::io::Write as _;

    let io = |err| Error::Io(format!("write {}: {err}", path.display()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path).map_err(io)?;
    file.write_all(content).map_err(io)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::mail::fake::Fake;

    #[test]
    fn specs_name_a_folder_and_a_positive_count() {
        assert_eq!(
            parse_spec("Junk=200"),
            Ok(Spec {
                folder: "Junk".into(),
                limit: 200
            })
        );
        assert_eq!(parse_spec("a=b/c d=5").unwrap().folder, "a=b/c d");
        for bad in ["Junk", "=5", "Junk=0", "Junk=-1", "Junk=x"] {
            assert!(parse_spec(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_latest_messages_are_written_privately() {
        let mut fake = Fake::new(&["Junk"]);
        for n in 0..5 {
            fake.deliver("Junk", &format!("Subject: {n}\r\n\r\nbody"), &[]);
        }
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("spam").join("Junk");

        let written = fetch_folder(
            &mut fake,
            &Spec {
                folder: "Junk".into(),
                limit: 3,
            },
            &target,
        )
        .await
        .unwrap();

        let mut names: Vec<String> = std::fs::read_dir(&target)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!((written, names.len()), (3, 3));
        let first = target.join(&names[0]);
        assert!(
            std::fs::read_to_string(&first)
                .unwrap()
                .contains("Subject: 2")
        );
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[tokio::test]
    async fn a_missing_folder_is_an_error() {
        let mut fake = Fake::new(&[]);
        let dir = tempfile::tempdir().unwrap();

        let err = fetch_folder(
            &mut fake,
            &Spec {
                folder: "Nope".into(),
                limit: 1,
            },
            dir.path(),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("list Nope"), "{err}");
    }
}
