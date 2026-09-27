use crate::scan::{Entry, Report};
use std::{collections::BTreeSet, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Video,
    Image,
    Audio,
    Document,
    Archive,
    Application,
    AppData,
    Cache,
    Other,
}

impl Category {
    pub const ALL: [Self; 9] = [
        Self::Video,
        Self::Image,
        Self::Audio,
        Self::Document,
        Self::Archive,
        Self::Application,
        Self::AppData,
        Self::Cache,
        Self::Other,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Self::Video => "影片",
            Self::Image => "圖片",
            Self::Audio => "音訊",
            Self::Document => "文件",
            Self::Archive => "壓縮檔／安裝檔",
            Self::Application => "應用程式",
            Self::AppData => "App 資料",
            Self::Cache => "快取／記錄",
            Self::Other => "其他",
        }
    }
}

pub fn classify(path: &Path) -> Category {
    let parts: Vec<_> = path
        .components()
        .map(|p| p.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    if parts.iter().any(|p| p.ends_with(".app")) {
        return Category::Application;
    }
    if parts
        .windows(2)
        .any(|p| p[0] == "library" && matches!(p[1].as_str(), "caches" | "logs"))
    {
        return Category::Cache;
    }
    if parts.windows(2).any(|p| {
        p[0] == "library"
            && matches!(
                p[1].as_str(),
                "application support" | "containers" | "group containers" | "preferences"
            )
    }) {
        return Category::AppData;
    }
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    match extension.as_str() {
        "mp4" | "mkv" | "mov" | "avi" | "m4v" | "webm" | "wmv" | "flv" | "mpeg" | "mpg" | "mts"
        | "m2ts" => Category::Video,
        "jpg" | "jpeg" | "png" | "gif" | "heic" | "heif" | "tif" | "tiff" | "webp" | "svg"
        | "raw" | "dng" | "psd" | "avif" => Category::Image,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "aiff" | "alac" => Category::Audio,
        "pdf" | "txt" | "md" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "csv" | "rtf"
        | "pages" | "numbers" | "key" | "epub" => Category::Document,
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "dmg" | "pkg" | "iso" => {
            Category::Archive
        }
        "log" | "cache" => Category::Cache,
        _ => Category::Other,
    }
}

#[derive(Clone, Copy, Default)]
pub struct CategoryTotal {
    pub logical: u64,
    pub allocated: u64,
    pub count: usize,
}

pub fn category_totals(
    report: &Report,
    categories: &[Category],
    current: usize,
) -> [CategoryTotal; 9] {
    let mut totals = [CategoryTotal::default(); 9];
    let mut stack = vec![current];
    while let Some(id) = stack.pop() {
        let entry = &report.entries[id];
        if entry.directory {
            stack.extend(&entry.children);
        } else {
            let total = &mut totals[categories[id] as usize];
            total.logical = total.logical.saturating_add(entry.logical);
            total.allocated = total.allocated.saturating_add(entry.allocated);
            total.count += 1;
        }
    }
    totals
}

pub fn bytes(entry: &Entry, allocated: bool) -> u64 {
    if allocated {
        entry.allocated
    } else {
        entry.logical
    }
}

pub fn sort_rows(rows: &mut [usize], entries: &[Entry], allocated: bool, ascending: bool) {
    rows.sort_unstable_by(|&a, &b| {
        let order = bytes(&entries[a], allocated).cmp(&bytes(&entries[b], allocated));
        (if ascending { order } else { order.reverse() })
            .then_with(|| entries[a].path.cmp(&entries[b].path))
    });
}

#[derive(Default)]
pub struct Selection {
    pub ids: BTreeSet<usize>,
    pub anchor: Option<usize>,
}
impl Selection {
    pub fn clear(&mut self) {
        self.ids.clear();
        self.anchor = None;
    }
    pub fn click(&mut self, id: usize, rows: &[usize], additive: bool, range: bool) {
        if range {
            if let Some((start, end)) = self
                .anchor
                .and_then(|a| rows.iter().position(|&r| r == a))
                .zip(rows.iter().position(|&r| r == id))
            {
                if !additive {
                    self.ids.clear();
                }
                self.ids
                    .extend(rows[start.min(end)..=start.max(end)].iter().copied());
                return;
            }
        }
        if additive {
            if !self.ids.remove(&id) {
                self.ids.insert(id);
            }
        } else {
            self.ids.clear();
            self.ids.insert(id);
        }
        self.anchor = Some(id);
    }
}

/// If both a folder and a descendant are selected, operate on the folder only.
pub fn batch_roots(report: &Report, selection: &BTreeSet<usize>) -> Vec<usize> {
    selection
        .iter()
        .copied()
        .filter(|&id| {
            let mut parent = report.entries[id].parent;
            while let Some(p) = parent {
                if selection.contains(&p) {
                    return false;
                }
                parent = report.entries[p].parent;
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{scan, Progress};
    use std::fs;

    #[test]
    fn categories_handle_uppercase_extensions_and_app_bundles() {
        assert_eq!(classify(Path::new("/a/CLIP.MP4")), Category::Video);
        assert_eq!(
            classify(Path::new("/Applications/Example.app/Contents/icon.png")),
            Category::Application
        );
        assert_eq!(
            classify(Path::new("/Users/a/Library/Caches/movie.mp4")),
            Category::Cache
        );
        assert_eq!(
            classify(Path::new("/Users/a/Library/Application Support/Example/db")),
            Category::AppData
        );
        assert_eq!(classify(Path::new("/a/unknown")), Category::Other);
    }

    #[test]
    fn totals_sorting_and_batch_dedup_follow_real_tree() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("folder")).unwrap();
        fs::write(dir.path().join("folder/movie.MP4"), [0; 32]).unwrap();
        fs::write(dir.path().join("notes.pdf"), [0; 12]).unwrap();
        let report = scan(dir.path(), &Progress::default()).unwrap();
        let categories: Vec<_> = report.entries.iter().map(|e| classify(&e.path)).collect();
        let totals = category_totals(&report, &categories, 0);
        assert_eq!(totals[Category::Video as usize].logical, 32);
        assert_eq!(
            totals.iter().map(|t| t.logical).sum::<u64>(),
            report.entries[0].logical
        );
        assert_eq!(
            totals.iter().map(|t| t.allocated).sum::<u64>(),
            report.entries[0].allocated
        );
        let mut rows = report.entries[0].children.clone();
        sort_rows(&mut rows, &report.entries, false, true);
        assert_eq!(report.entries[rows[0]].logical, 12);
        sort_rows(&mut rows, &report.entries, false, false);
        assert_eq!(report.entries[rows[0]].logical, 32);
        sort_rows(&mut rows, &report.entries, true, false);
        assert!(rows
            .windows(2)
            .all(|r| report.entries[r[0]].allocated >= report.entries[r[1]].allocated));
        let folder = report
            .entries
            .iter()
            .position(|e| e.path.ends_with("folder"))
            .unwrap();
        let movie = report
            .entries
            .iter()
            .position(|e| e.path.ends_with("movie.MP4"))
            .unwrap();
        assert_eq!(
            batch_roots(&report, &BTreeSet::from([folder, movie])),
            vec![folder]
        );
    }

    #[test]
    fn selection_supports_toggle_and_range_in_display_order() {
        let mut selection = Selection::default();
        let rows = [5, 3, 8, 2];
        selection.click(3, &rows, false, false);
        selection.click(2, &rows, false, true);
        assert_eq!(selection.ids, BTreeSet::from([3, 8, 2]));
        selection.click(8, &rows, true, false);
        assert_eq!(selection.ids, BTreeSet::from([3, 2]));
        selection.click(5, &rows, false, false);
        assert_eq!(selection.ids, BTreeSet::from([5]));
    }
}
