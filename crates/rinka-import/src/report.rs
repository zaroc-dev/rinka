//! Conversion reports.
//!
//! Every source object (and each effect on it) becomes an item with a
//! status and a mapping to the native object it became. The overall status
//! is the worst item status: anything short of fully supported blocks a
//! normal import, so behavior is never dropped silently.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Supported,
    Partial,
    Unsupported,
}

#[derive(Clone, Debug, Serialize)]
pub struct Item {
    /// Source object, e.g. `object 13` or `object 13 › effect 30`.
    pub source: String,
    pub name: String,
    /// e.g. `image layer`, `effect`, `particle system`.
    pub kind: String,
    pub status: Status,
    /// Native object it became, e.g. `layer 13`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

impl Item {
    pub fn new(
        source: impl Into<String>,
        name: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            source: source.into(),
            name: name.into(),
            kind: kind.into(),
            status: Status::Supported,
            native: None,
            notes: Vec::new(),
        }
    }

    /// Records a loss; lowers the status to partial.
    pub fn partial(&mut self, note: impl Into<String>) {
        self.status = self.status.max(Status::Partial);
        self.notes.push(note.into());
    }

    pub fn unsupported(&mut self, note: impl Into<String>) {
        self.status = Status::Unsupported;
        self.notes.push(note.into());
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Source {
    pub kind: &'static str,
    pub id: Option<String>,
    pub title: String,
    /// Wallpaper Engine item type: scene, video, web, application.
    pub item_type: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub supported: usize,
    pub partial: usize,
    pub unsupported: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub format: &'static str,
    pub version: u32,
    pub source: Source,
    pub status: Status,
    pub summary: Summary,
    pub items: Vec<Item>,
    /// Whether a project was written.
    pub written: bool,
}

impl Report {
    pub fn new(source: Source) -> Self {
        Self {
            format: "rinka.import-report",
            version: 1,
            source,
            status: Status::Supported,
            summary: Summary::default(),
            items: Vec::new(),
            written: false,
        }
    }

    pub fn push(&mut self, item: Item) {
        self.items.push(item);
        self.finish();
    }

    pub fn finish(&mut self) {
        self.status = self
            .items
            .iter()
            .map(|i| i.status)
            .max()
            .unwrap_or(Status::Supported);
        self.summary = Summary::default();
        for i in &self.items {
            match i.status {
                Status::Supported => self.summary.supported += 1,
                Status::Partial => self.summary.partial += 1,
                Status::Unsupported => self.summary.unsupported += 1,
            }
        }
    }
}
