//! Wallpaper Engine `.pkg` archives (`PKGV0001`…`PKGV0023` observed).
//!
//! Layout, all integers little-endian u32:
//! `len, magic` · `count` · `count × (len, name, offset, size)` · data.
//! Offsets are relative to the end of the entry table.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum PkgError {
    #[error("not a scene package ({0})")]
    Magic(String),
    #[error("package is truncated or corrupt")]
    Truncated,
}

pub struct Package {
    pub version: String,
    files: BTreeMap<String, (usize, usize)>,
    data: Vec<u8>,
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, PkgError> {
        let b = self
            .data
            .get(self.pos..self.pos + 4)
            .ok_or(PkgError::Truncated)?;
        self.pos += 4;
        Ok(u32::from_le_bytes(b.try_into().expect("4 bytes")))
    }

    fn string(&mut self) -> Result<String, PkgError> {
        let len = self.u32()? as usize;
        let b = self
            .data
            .get(self.pos..self.pos + len)
            .ok_or(PkgError::Truncated)?;
        self.pos += len;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}

impl Package {
    pub fn parse(data: Vec<u8>) -> Result<Self, PkgError> {
        let mut r = Reader {
            data: &data,
            pos: 0,
        };
        let version = r.string()?;
        if !version.starts_with("PKGV") {
            return Err(PkgError::Magic(version));
        }
        let count = r.u32()?;
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let name = r.string()?;
            let offset = r.u32()? as usize;
            let size = r.u32()? as usize;
            entries.push((name, offset, size));
        }
        let base = r.pos;
        let mut files = BTreeMap::new();
        for (name, offset, size) in entries {
            let start = base + offset;
            if start + size > data.len() {
                return Err(PkgError::Truncated);
            }
            files.insert(name, (start, size));
        }
        Ok(Self {
            version,
            files,
            data,
        })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.files
            .get(name)
            .map(|&(start, size)| &self.data[start..start + size])
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let s = |out: &mut Vec<u8>, v: &str| {
            out.extend((v.len() as u32).to_le_bytes());
            out.extend(v.as_bytes());
        };
        s(&mut out, "PKGV0023");
        out.extend((files.len() as u32).to_le_bytes());
        let mut offset = 0u32;
        for (name, data) in files {
            s(&mut out, name);
            out.extend(offset.to_le_bytes());
            out.extend((data.len() as u32).to_le_bytes());
            offset += data.len() as u32;
        }
        for (_, data) in files {
            out.extend(*data);
        }
        out
    }

    #[test]
    fn reads_entries() {
        let pkg =
            Package::parse(build(&[("scene.json", b"{}"), ("materials/a.tex", b"xyz")])).unwrap();
        assert_eq!(pkg.version, "PKGV0023");
        assert_eq!(pkg.get("scene.json"), Some(&b"{}"[..]));
        assert_eq!(pkg.get("materials/a.tex"), Some(&b"xyz"[..]));
        assert!(pkg.get("missing").is_none());
    }

    #[test]
    fn rejects_truncation_and_other_files() {
        let mut data = build(&[("a", b"0123456789")]);
        data.truncate(data.len() - 1);
        assert!(matches!(Package::parse(data), Err(PkgError::Truncated)));
        assert!(matches!(
            Package::parse(build(&[]).split_off(0)).map(|p| p.len()),
            Ok(0)
        ));
        let mut other = Vec::new();
        other.extend(4u32.to_le_bytes());
        other.extend(b"RIFF");
        assert!(matches!(Package::parse(other), Err(PkgError::Magic(_))));
    }
}
