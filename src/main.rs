mod scan;
mod view;
use view::{Category, CategoryTotal, Selection};

use eframe::egui::{self, Color32, FontId, RichText, Vec2};
use scan::{Entry, Progress, Report};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{atomic::Ordering, mpsc, Arc},
    time::{Duration, Instant},
};

const INK: Color32 = Color32::from_rgb(31, 48, 62);
const MUTED: Color32 = Color32::from_rgb(107, 123, 132);
const TEAL: Color32 = Color32::from_rgb(25, 125, 122);
const COLORS: [Color32; 6] = [
    TEAL,
    Color32::from_rgb(71, 150, 159),
    Color32::from_rgb(92, 122, 173),
    Color32::from_rgb(139, 132, 181),
    Color32::from_rgb(203, 155, 100),
    Color32::from_rgb(124, 160, 144),
];

fn size(bytes: u64) -> String {
    let mut value = bytes as f64;
    let units = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut unit = 0;
    while value >= 1000.0 && unit < units.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

fn name(entry: &Entry) -> String {
    entry
        .path
        .file_name()
        .unwrap_or(entry.path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

struct App {
    report: Option<Report>,
    receiver: Option<mpsc::Receiver<Result<Report, String>>>,
    trash_receiver: Option<mpsc::Receiver<scan::TrashOutcome>>,
    trash_result: Option<scan::TrashOutcome>,
    show_trash_result: bool,
    pending_trash: Vec<Entry>,
    pending_root: Option<PathBuf>,
    permanent: bool,
    permanent_ack: bool,
    progress: Arc<Progress>,
    root: Option<PathBuf>,
    current: usize,
    selection: Selection,
    ascending: bool,
    category: BTreeSet<Category>,
    delete_progress: Arc<scan::DeleteProgress>,
    delete_started: Instant,
    categories: Vec<Category>,
    category_totals: [CategoryTotal; 9],
    view_revision: u64,
    allocated: bool,
    large_files: bool,
    filter: String,
    rows: Vec<usize>,
    message: String,
    started: Instant,
    elapsed: Duration,
    confirm: bool,
    show_errors: bool,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        for path in [
            "/System/Library/Fonts/STHeiti Light.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ] {
            if let Ok(data) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert("chinese".into(), egui::FontData::from_owned(data).into());
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts
                        .families
                        .entry(family)
                        .or_default()
                        .push("chinese".into());
                }
                break;
            }
        }
        ctx.set_fonts(fonts);
        let mut style = (*ctx.style()).clone();
        style.visuals = egui::Visuals::light();
        style.visuals.override_text_color = Some(INK);
        style.visuals.panel_fill = Color32::from_rgb(249, 250, 248);
        style.visuals.selection.bg_fill = Color32::from_rgb(207, 234, 229);
        style.visuals.selection.stroke = egui::Stroke::new(1.0_f32, TEAL);
        style.spacing.item_spacing = Vec2::new(10.0, 10.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(14.0));
        ctx.set_style(style);
        Self {
            report: None,
            receiver: None,
            trash_receiver: None,
            trash_result: None,
            show_trash_result: false,
            pending_trash: vec![],
            pending_root: None,
            permanent: false,
            permanent_ack: false,
            progress: Arc::new(Progress::default()),
            root: None,
            current: 0,
            selection: Selection::default(),
            ascending: false,
            category: BTreeSet::new(),
            delete_progress: Arc::new(scan::DeleteProgress::default()),
            delete_started: Instant::now(),
            categories: vec![],
            category_totals: [CategoryTotal::default(); 9],
            view_revision: 0,
            allocated: false,
            large_files: false,
            filter: String::new(),
            rows: vec![],
            message: String::new(),
            started: Instant::now(),
            elapsed: Duration::ZERO,
            confirm: false,
            show_errors: false,
        }
    }

    fn busy(&self) -> bool {
        self.receiver.is_some() || self.trash_receiver.is_some()
    }

    fn start(&mut self, path: PathBuf, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        self.progress = Arc::new(Progress::default());
        self.root = Some(path.clone());
        self.report = None;
        self.category.clear();
        self.categories.clear();
        self.rows.clear();
        self.current = 0;
        self.selection.clear();
        self.filter.clear();
        self.confirm = false;
        self.started = Instant::now();
        self.message = "正在掃描；不會修改檔案".into();
        let progress = self.progress.clone();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(scan::scan(&path, &progress));
            ctx.request_repaint();
        });
    }

    fn pick(&mut self, ctx: &egui::Context) {
        if let Some(path) = rfd::FileDialog::new()
            .set_title("選擇要分析的資料夾")
            .pick_folder()
        {
            self.start(path, ctx);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.receiver {
            match rx.try_recv() {
                Ok(result) => {
                    self.receiver = None;
                    self.elapsed = self.started.elapsed();
                    match result {
                        Ok(report) => {
                            self.message = if report.cancelled {
                                "掃描已取消，以下為部分結果".into()
                            } else if report.error_count > 0 {
                                format!(
                                    "掃描結束，{} 個項目無法讀取；結果不完整",
                                    report.error_count
                                )
                            } else {
                                "掃描完成".into()
                            };
                            self.root = Some(report.entries[0].path.clone());
                            self.categories = report
                                .entries
                                .iter()
                                .map(|entry| view::classify(&entry.path))
                                .collect();
                            self.category_totals =
                                view::category_totals(&report, &self.categories, 0);
                            self.report = Some(report);
                            self.rebuild_rows();
                        }
                        Err(error) => self.message = format!("掃描失敗：{error}"),
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.receiver = None;
                    self.message = "掃描執行緒中斷，請重新掃描".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.trash_receiver {
            match rx.try_recv() {
                Ok(result) => {
                    self.trash_receiver = None;
                    let permanent = result.permanent;
                    let action = if permanent {
                        "永久刪除"
                    } else {
                        "移到垃圾桶"
                    };
                    let moved = result.moved.len();
                    let failed = result.failed.len();
                    self.trash_result = Some(result);
                    self.show_trash_result = true;
                    if moved > 0 || permanent {
                        if let Some(root) = self.root.clone() {
                            self.start(root, ctx);
                        }
                    }
                    self.message = format!(
                        "已{action} {moved} 項 · 失敗 {failed} 項{}",
                        if moved > 0 || permanent {
                            "；正在更新結果"
                        } else {
                            ""
                        }
                    );
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.trash_receiver = None;
                    self.message = "操作中斷，請檢查 Finder 並重新掃描".into();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn bytes(&self, entry: &Entry) -> u64 {
        if self.allocated {
            entry.allocated
        } else {
            entry.logical
        }
    }

    fn rebuild_rows(&mut self) {
        let Some(report) = &self.report else {
            return;
        };
        let filter = self.filter.to_lowercase();
        let mut rows: Vec<usize> = if self.large_files {
            report
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    !e.directory && e.path.starts_with(&report.entries[self.current].path)
                })
                .map(|(i, _)| i)
                .collect()
        } else {
            report.entries[self.current].children.clone()
        };
        rows.retain(|&i| name(&report.entries[i]).to_lowercase().contains(&filter));
        if !self.category.is_empty() {
            rows.retain(|&id| self.category.contains(&self.categories[id]));
        }
        view::sort_rows(&mut rows, &report.entries, self.allocated, self.ascending);
        self.rows = rows;
        self.view_revision += 1;
        self.selection.anchor = None;
    }

    fn navigate(&mut self, id: usize) {
        self.current = id;
        self.selection.clear();
        self.filter.clear();
        if let Some(report) = &self.report {
            self.category_totals = view::category_totals(report, &self.categories, id);
        }
        self.rebuild_rows();
    }

    fn select(&mut self, id: usize, ctx: &egui::Context, checkbox: bool) {
        let modifiers = ctx.input(|i| i.modifiers);
        self.selection.click(
            id,
            &self.rows,
            checkbox || modifiers.command || modifiers.ctrl,
            modifiers.shift && !checkbox,
        );
    }

    fn request_trash(&mut self) {
        self.permanent = false;
        self.permanent_ack = false;
        self.pending_root = self.root.clone();
        if let Some(report) = &self.report {
            self.pending_trash = view::batch_roots(report, &self.selection.ids)
                .iter()
                .map(|&id| report.entries[id].clone())
                .collect();
            self.confirm = !self.pending_trash.is_empty();
        }
    }

    fn reveal(&mut self, id: usize) {
        let path = &self.report.as_ref().unwrap().entries[id].path;
        match std::process::Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .status()
        {
            Ok(status) if status.success() => {}
            _ => self.message = "無法在 Finder 顯示此項目".into(),
        }
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(205.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(235, 241, 238))
                    .inner_margin(20),
            )
            .show(ctx, |ui| {
                ui.add_space(16.0);
                ui.label(RichText::new("SPACE LENS").size(13.0).color(TEAL).strong());
                ui.label(RichText::new("空間透鏡").size(27.0).strong());
                ui.label(RichText::new("看見空間，整理日常").color(MUTED).size(13.0));
                ui.add_space(28.0);
                if ui
                    .add_enabled(
                        !self.busy(),
                        egui::Button::new("＋  選擇資料夾").min_size(Vec2::new(165.0, 38.0)),
                    )
                    .clicked()
                {
                    self.pick(ctx);
                }
                ui.add_space(22.0);
                ui.label(RichText::new("快速掃描").small().color(MUTED));
                let home = std::env::var_os("HOME").map(PathBuf::from);
                if let Some(home) = home {
                    for (label, path) in [
                        ("下載項目", home.join("Downloads")),
                        ("桌面", home.join("Desktop")),
                        ("文件", home.join("Documents")),
                        ("個人主目錄", home),
                    ] {
                        if ui
                            .add_enabled(!self.busy(), egui::Button::new(label).frame(false))
                            .clicked()
                        {
                            self.start(path, ctx);
                        }
                    }
                }
                ui.add_space(26.0);
                ui.separator();
                ui.label(RichText::new("只在這台 Mac 上處理").size(13.0).color(TEAL));
                ui.label(
                    RichText::new("不會上傳檔案或掃描結果。\n刪除操作會先請你確認。")
                        .size(12.0)
                        .color(MUTED),
                );
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(RichText::new("本機預覽版  0.4.0").small().color(MUTED));
                });
            });
    }

    fn inspector(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("inspector")
            .exact_height(150.0)
            .frame(egui::Frame::new().fill(Color32::WHITE).inner_margin(16))
            .show(ctx, |ui| {
                ui.add(egui::Label::new(RichText::new(&self.message).color(TEAL)).truncate())
                    .on_hover_text(&self.message);
                if self.trash_receiver.is_some() {
                    let p = &self.delete_progress;
                    let completed = p.completed.load(Ordering::Relaxed);
                    let total = p.total.load(Ordering::Relaxed);
                    ui.add(egui::ProgressBar::new(completed as f32 / total.max(1) as f32).animate(true).text(format!("已處理 {completed} / {total} 個所選項目")));
                    ui.horizontal(|ui| { ui.spinner(); ui.label(format!("成功 {} · 失敗 {} · 已用 {:.0} 秒", p.succeeded.load(Ordering::Relaxed), p.failed.load(Ordering::Relaxed), self.delete_started.elapsed().as_secs_f32())); });
                    if let Ok(path) = p.current.try_lock() { ui.add(egui::Label::new(format!("目前：{path}")).truncate()).on_hover_text(path.as_str()); }
                    ui.label(RichText::new("按所選項目計算；單一資料夾須全部完成才前進，等待 NAS 回應時仍會持續計時。").small().color(MUTED));
                    return;
                }
                let Some(report) = &self.report else {
                    return;
                };
                let roots = view::batch_roots(report, &self.selection.ids);
                if roots.is_empty() {
                    ui.label(
                        RichText::new("勾選多個項目，或使用 Command 點選／Shift 範圍選取。")
                            .color(MUTED),
                    );
                    return;
                }
                let total = roots
                    .iter()
                    .map(|&id| self.bytes(&report.entries[id]))
                    .sum::<u64>();
                let single = if roots.len() == 1 {
                    Some(roots[0])
                } else {
                    None
                };
                ui.horizontal(|ui| {
                    ui.strong(format!(
                        "已選 {} 項 · {}",
                        self.selection.ids.len(),
                        size(total)
                    ));
                    if let Some(id) = single {
                        ui.add(egui::Label::new(name(&report.entries[id])).truncate());
                    } else {
                        ui.label("移動資料夾會包含其中所有內容");
                    }
                });
                if let Some(id) = single {
                    let path = report.entries[id].path.display().to_string();
                    ui.add(egui::Label::new(RichText::new(&path).small().color(MUTED)).truncate())
                        .on_hover_text(path);
                }
                ui.horizontal(|ui| {
                    if let Some(id) = single {
                        if ui
                            .add_enabled(!self.busy(), egui::Button::new("在 Finder 顯示"))
                            .clicked()
                        {
                            self.reveal(id);
                        }
                    }
                    if ui
                        .add_enabled(
                            !self.busy(),
                            egui::Button::new(
                                RichText::new(format!("將 {} 項移到垃圾桶…", roots.len()))
                                    .color(Color32::from_rgb(161, 73, 60)),
                            ),
                        )
                        .clicked()
                    {
                        self.request_trash();
                    }
                    if ui
                        .add_enabled(
                            !self.busy(),
                            egui::Button::new(
                                RichText::new("永久刪除所選項目…")
                                    .color(Color32::from_rgb(161, 50, 45)),
                            ),
                        )
                        .clicked()
                    {
                        self.request_trash();
                        self.permanent = true;
                    }
                    if ui
                        .add_enabled(!self.busy(), egui::Button::new("取消選取"))
                        .clicked()
                    {
                        self.selection.clear();
                    }
                });
            });
    }

    fn category_bar(&mut self, ui: &mut egui::Ui) -> bool {
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("空間類別（可複選）").strong());
            if ui
                .selectable_label(self.category.is_empty(), "全部")
                .clicked()
                && !self.category.is_empty()
            {
                self.category.clear();
                changed = true;
            }
            for category in Category::ALL {
                let total = self.category_totals[category as usize];
                if total.count == 0 {
                    continue;
                }
                let bytes = if self.allocated {
                    total.allocated
                } else {
                    total.logical
                };
                if ui
                    .selectable_label(
                        self.category.contains(&category),
                        format!("{} {}", category.label(), size(bytes)),
                    )
                    .on_hover_text(format!(
                        "{} 個檔案；可複選多種類別，再按全選符合類別的檔案",
                        total.count
                    ))
                    .clicked()
                {
                    if !self.category.remove(&category) {
                        self.category.insert(category);
                    }
                    self.large_files = true;
                    changed = true;
                }
            }
        });
        ui.label(
            RichText::new("目前資料夾的完整類別統計，依副檔名與路徑推估；不代表程序即時用量。")
                .size(12.0)
                .color(MUTED),
        );
        changed
    }

    fn content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.heading("儲存空間總覽");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.root.is_some() && !self.busy(),
                        egui::Button::new("重新掃描"),
                    )
                    .clicked()
                {
                    self.start(self.root.clone().unwrap(), ctx);
                }
            });
        });
        ui.add_space(10.0);
        if self.receiver.is_some() {
            ui.add_space(50.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.heading("正在探索資料夾…");
            });
            ui.label(format!(
                "已找到 {} 個檔案 · {} 個資料夾 · {}",
                self.progress.files.load(Ordering::Relaxed),
                self.progress.folders.load(Ordering::Relaxed),
                size(self.progress.bytes.load(Ordering::Relaxed))
            ));
            ui.label(
                RichText::new(format!(
                    "已用 {:.1} 秒 · 總檔案數未知，暫不顯示百分比",
                    self.started.elapsed().as_secs_f32()
                ))
                .color(MUTED),
            );
            if let Some(path) = &self.root {
                ui.label(path.display().to_string());
            }
            if ui
                .add_enabled(
                    !self.progress.cancel.load(Ordering::Relaxed),
                    egui::Button::new("取消掃描"),
                )
                .clicked()
            {
                self.progress.cancel.store(true, Ordering::Relaxed);
            }
            return;
        }
        let Some(report) = &self.report else {
            ui.add_space(65.0);
            ui.label(RichText::new("空間都去哪裡了？").size(32.0).strong());
            ui.add_space(12.0);
            ui.label("從一個資料夾開始，找出占空間的檔案。\n用圖表看比例，用清單查細節。");
            ui.add_space(20.0);
            if ui
                .button(RichText::new("選擇資料夾開始").size(17.0))
                .clicked()
            {
                self.pick(ctx);
            }
            ui.add_space(35.0);
            ui.label(
                RichText::new("01  選擇範圍       02  查看分布       03  確認後整理").color(MUTED),
            );
            return;
        };
        let current = &report.entries[self.current];
        let total = self.bytes(current);
        let path = current.path.display().to_string();
        let parent = current.parent;
        let error_count = report.error_count;
        let skipped = report.skipped_links + report.skipped_mounts;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(parent.is_some(), egui::Button::new("← 上一層"))
                .clicked()
            {
                self.navigate(parent.unwrap());
            }
            if ui.button("掃描根目錄").clicked() {
                self.navigate(0);
            }
        });
        ui.add(egui::Label::new(RichText::new(path.clone()).small().color(MUTED)).truncate())
            .on_hover_text(path);
        ui.horizontal(|ui| {
            ui.label(RichText::new(size(total)).size(34.0).strong());
            ui.label(
                RichText::new(format!(
                    "目前資料夾  ·  掃描耗時 {:.1} 秒",
                    self.elapsed.as_secs_f32()
                ))
                .color(MUTED),
            );
        });
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            changed |= ui
                .selectable_value(&mut self.allocated, false, "檔案大小")
                .changed();
            changed |= ui
                .selectable_value(&mut self.allocated, true, "配置空間（估計）")
                .changed();
            ui.separator();
            if ui
                .button(format!("略過 {skipped} · 無法讀取 {error_count}"))
                .clicked()
            {
                self.show_errors = true;
            }
        });
        ui.label(
            RichText::new("配置空間不等於可釋放空間；APFS 共享資料、快照與雲端檔案可能造成差異。")
                .size(12.0)
                .color(MUTED),
        );
        let mut clear_selection = self.category_bar(ui);
        changed |= clear_selection;
        ui.horizontal_wrapped(|ui| {
            if ui
                .selectable_value(&mut self.large_files, false, "資料夾內容")
                .changed()
            {
                self.category.clear();
                changed = true;
                clear_selection = true;
            }
            if ui
                .selectable_value(&mut self.large_files, true, "所有大型檔案")
                .changed()
            {
                changed = true;
                clear_selection = true;
            }
            if ui
                .add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("搜尋名稱…")
                        .desired_width(155.0),
                )
                .changed()
            {
                changed = true;
                clear_selection = true;
            }
            ui.label("容量排序");
            changed |= egui::ComboBox::from_id_salt("size-sort")
                .selected_text(if self.ascending {
                    "升冪：小 → 大"
                } else {
                    "降冪：大 → 小"
                })
                .show_ui(ui, |ui| {
                    let a = ui
                        .selectable_value(&mut self.ascending, false, "降冪：大 → 小")
                        .changed();
                    let b = ui
                        .selectable_value(&mut self.ascending, true, "升冪：小 → 大")
                        .changed();
                    a || b
                })
                .inner
                .unwrap_or(false);
        });
        if clear_selection {
            self.selection.clear();
        }
        if changed {
            self.rebuild_rows();
        }
        self.map(ui);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.rows.is_empty(),
                    egui::Button::new(if self.category.is_empty() {
                        "全選顯示項目"
                    } else {
                        "全選符合類別的檔案"
                    }),
                )
                .clicked()
            {
                self.selection.ids = self.rows.iter().copied().collect();
            }
            if ui.button("取消選取").clicked() {
                self.selection.clear();
            }
            ui.label(
                RichText::new(format!(
                    "{} 個項目 · {} · 已選 {} 項 · 雙擊資料夾進入",
                    self.rows.len(),
                    if self.ascending {
                        "小 → 大"
                    } else {
                        "大 → 小"
                    },
                    self.selection.ids.len()
                ))
                .small()
                .color(MUTED),
            );
        });
        self.table(ui);
    }

    fn map(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.report else {
            return;
        };
        // Aggregate small entries into one labeled block; map area always accounts for all rows.
        let mut positive: Vec<_> = self
            .rows
            .iter()
            .copied()
            .filter(|&id| self.bytes(&report.entries[id]) > 0)
            .collect();
        // Keep the largest tiles visible even when the table is sorted ascending.
        view::sort_rows(&mut positive, &report.entries, self.allocated, false);
        let mut tiles: Vec<(Option<usize>, u64)> = positive
            .iter()
            .take(18)
            .map(|&id| (Some(id), self.bytes(&report.entries[id])))
            .collect();
        let other: u64 = positive
            .iter()
            .skip(18)
            .map(|&id| self.bytes(&report.entries[id]))
            .sum();
        if other > 0 {
            tiles.push((None, other));
        }
        let height = (ui.available_height() * 0.32).clamp(60.0, 160.0);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), height),
            egui::Sense::hover(),
        );
        if tiles.is_empty() {
            ui.painter()
                .rect_filled(rect, 8.0, Color32::from_rgb(233, 238, 235));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "沒有符合條件的非零項目",
                FontId::proportional(14.0),
                MUTED,
            );
            return;
        }
        let mut rectangles = vec![];
        partition(&tiles, rect, &mut rectangles);
        let mut navigate = None;
        let mut clicked = None;
        for (index, (id, bytes, rect)) in rectangles.into_iter().enumerate() {
            let rect = rect.shrink(2.0);
            if !rect.is_positive() {
                continue;
            }
            let response = ui.interact(rect, ui.id().with(("tile", index)), egui::Sense::click());
            let label = id
                .map(|i| name(&report.entries[i]))
                .unwrap_or_else(|| "其他項目".into());
            let color = if id.is_some_and(|id| self.selection.ids.contains(&id)) {
                INK
            } else {
                COLORS[index % COLORS.len()]
            };
            ui.painter().rect_filled(
                rect,
                6.0,
                if response.hovered() {
                    color.gamma_multiply(0.85)
                } else {
                    color
                },
            );
            if rect.width() > 65.0 && rect.height() > 38.0 {
                let painter = ui.painter().with_clip_rect(rect.shrink(6.0));
                painter.text(
                    rect.min + Vec2::new(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    &label,
                    FontId::proportional(14.0),
                    Color32::WHITE,
                );
                painter.text(
                    rect.min + Vec2::new(10.0, 30.0),
                    egui::Align2::LEFT_TOP,
                    size(bytes),
                    FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
            if response.clicked() {
                clicked = id;
            }
            if response.double_clicked() {
                if let Some(id) = id {
                    if report.entries[id].directory {
                        navigate = Some(id);
                    }
                }
            }
            response.on_hover_text(format!("{label}\n{}", size(bytes)));
        }
        if let Some(id) = clicked {
            self.select(id, ui.ctx(), false);
        }
        if let Some(id) = navigate {
            self.navigate(id);
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        let Some(report) = &self.report else {
            return;
        };
        let mut navigate = None;
        let mut clicked = None;
        let allocated = self.allocated;
        egui::ScrollArea::vertical()
            .id_salt(("file-list", self.view_revision))
            .auto_shrink([false, false])
            .show_rows(ui, 33.0, self.rows.len(), |ui, range| {
                for row in range {
                    let id = self.rows[row];
                    let entry = &report.entries[id];
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 33.0),
                        egui::Sense::hover(),
                    );
                    let checkbox_rect = egui::Rect::from_min_size(rect.min, Vec2::new(28.0, 33.0));
                    let mut checked = self.selection.ids.contains(&id);
                    let checkbox =
                        ui.put(checkbox_rect, egui::Checkbox::without_text(&mut checked));
                    checkbox.widget_info(|| {
                        egui::WidgetInfo::selected(
                            egui::WidgetType::Checkbox,
                            ui.is_enabled(),
                            checked,
                            format!("選取 {}", name(entry)),
                        )
                    });
                    let hit_rect =
                        egui::Rect::from_min_max(rect.min + Vec2::new(32.0, 0.0), rect.max);
                    let response =
                        ui.interact(hit_rect, ui.id().with(("row", id)), egui::Sense::click());
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            ui.is_enabled(),
                            name(entry),
                        )
                    });
                    if checked || response.hovered() {
                        ui.painter()
                            .rect_filled(hit_rect, 5.0, Color32::from_rgb(225, 238, 232));
                    }
                    let painter = ui.painter();
                    let kind = if entry.directory {
                        "資料夾"
                    } else {
                        self.categories[id].label()
                    };
                    let text_rect = egui::Rect::from_min_max(
                        rect.min + Vec2::new(38.0, 0.0),
                        rect.max - Vec2::new(215.0, 0.0),
                    );
                    painter.with_clip_rect(text_rect).text(
                        text_rect.left_center(),
                        egui::Align2::LEFT_CENTER,
                        name(entry),
                        FontId::proportional(14.0),
                        INK,
                    );
                    painter.text(
                        rect.right_center() - Vec2::new(130.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        kind,
                        FontId::proportional(12.0),
                        MUTED,
                    );
                    painter.text(
                        rect.right_center() - Vec2::new(10.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        size(view::bytes(entry, allocated)),
                        FontId::proportional(14.0),
                        MUTED,
                    );
                    if checkbox.changed() {
                        clicked = Some((id, true));
                    } else if response.clicked() {
                        clicked = Some((id, false));
                    }
                    if response.double_clicked() && entry.directory {
                        navigate = Some(id);
                    }
                    response.on_hover_text(entry.path.display().to_string());
                }
            });
        if let Some((id, checkbox)) = clicked {
            self.select(id, ui.ctx(), checkbox);
        }
        if let Some(id) = navigate {
            self.navigate(id);
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.show_errors {
            egui::Window::new("掃描範圍與略過項目").open(&mut self.show_errors).default_width(620.0).show(ctx, |ui| {
                ui.label("不跟隨符號連結，不跨越磁碟。硬連結只計算一次。\n檔案總量會隨掃描期間的變更而變動。");
                if let Some(report) = &self.report {
                    ui.label(format!("符號連結 {} · 其他磁碟 {} · 讀取錯誤 {}", report.skipped_links, report.skipped_mounts, report.error_count));
                    ui.label("若遇到權限限制，可在「系統設定 → 隱私權與安全性 → 完整磁碟存取權」允許空間透鏡，再重新掃描。");
                    egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| { for error in &report.errors { ui.label(error); } });
                    if report.error_count > 50 { ui.label("僅顯示前 50 筆錯誤。"); }
                }
            });
        }
        if self.confirm {
            egui::Modal::new(egui::Id::new("trash-confirm")).show(ctx, |ui| {
                ui.set_width(570.0);
                let action = if self.permanent {
                    "永久刪除"
                } else {
                    "移到垃圾桶"
                };
                ui.heading(format!("{} 個項目：{action}？", self.pending_trash.len()));
                let total: u64 = self.pending_trash.iter().map(|e| self.bytes(e)).sum();
                ui.label(format!(
                    "掃描統計 {}；資料夾及其中所有內容都會一併處理。",
                    size(total)
                ));
                let folder_count = self
                    .pending_trash
                    .iter()
                    .filter(|entry| entry.directory)
                    .count();
                ui.label(format!(
                    "包含 {folder_count} 個資料夾、{} 個檔案。",
                    self.pending_trash.len() - folder_count
                ));
                ui.label(if folder_count > 0 {
                    "選取的資料夾本身與全部內容都會刪除。"
                } else {
                    "只刪除清單中的檔案，保留所在資料夾。"
                });
                ui.label("0 B 不保證資料夾沒有內容；掃描後新增的內容也包含在操作範圍。");
                egui::ScrollArea::vertical()
                    .id_salt("pending-trash")
                    .max_height(230.0)
                    .show(ui, |ui| {
                        for entry in &self.pending_trash {
                            ui.label(entry.path.display().to_string());
                        }
                    });
                if self.permanent {
                    ui.colored_label(
                        Color32::from_rgb(161, 50, 45),
                        "永久刪除不經垃圾桶，無法從垃圾桶復原。",
                    );
                    ui.checkbox(
                        &mut self.permanent_ack,
                        "我了解這些項目與資料夾內容將永久刪除",
                    );
                } else {
                    ui.label("可從 Finder 的垃圾桶將項目拖回原位置。不會清空垃圾桶。");
                    ui.label("NAS 若不支援垃圾桶，可在失敗結果中選擇永久刪除。");
                }
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.confirm = false;
                        self.pending_trash.clear();
                    }
                    if ui
                        .add_enabled(
                            !self.permanent || self.permanent_ack,
                            egui::Button::new(format!("確認{action}")),
                        )
                        .clicked()
                    {
                        if let Some(root) = self.pending_root.clone() {
                            let entries = std::mem::take(&mut self.pending_trash);
                            let permanent = self.permanent;
                            let (tx, rx) = mpsc::channel();
                            let ctx = ctx.clone();
                            self.delete_progress = Arc::new(scan::DeleteProgress::default());
                            self.delete_progress
                                .total
                                .store(entries.len() as u64, Ordering::Relaxed);
                            self.delete_started = Instant::now();
                            let progress = self.delete_progress.clone();
                            self.trash_receiver = Some(rx);
                            self.message = format!("正在{action} {} 個項目…", entries.len());
                            std::thread::spawn(move || {
                                let _ = tx.send(scan::delete_batch_progress(
                                    &root, &entries, permanent, &progress,
                                ));
                                ctx.request_repaint();
                            });
                        }
                        self.confirm = false;
                    }
                });
            });
        }
        if self.show_trash_result {
            let mut retry = false;
            let busy = self.busy();
            egui::Window::new("批次整理結果").open(&mut self.show_trash_result).default_width(620.0).show(ctx, |ui| {
                if let Some(result) = &self.trash_result {
                    let action = if result.permanent { "永久刪除" } else { "移到垃圾桶" };
                    ui.heading(format!("{action}：成功 {} 項 · 失敗 {} 項", result.moved.len(), result.failed.len()));
                    if result.permanent {
                        ui.label("成功項目已永久刪除。失敗資料夾可能已移除部分內容，請重新掃描確認。");
                    } else {
                        ui.label("成功項目已移到垃圾桶；失敗項目未自動改用永久刪除。");
                    }
                    if !result.failed_entries.is_empty() {
                        retry = ui.add_enabled(!busy, egui::Button::new(if result.permanent { "重試永久刪除失敗項目…" } else { "改為永久刪除失敗項目…" })).clicked();
                    }
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for path in &result.moved { ui.label(format!("已{action}：{}", path.display())); }
                        for (entry, error) in result.failed_entries.iter().zip(&result.failed) {
                            ui.colored_label(Color32::from_rgb(161, 73, 60), entry.path.display().to_string());
                            let summary = if error.contains("doesn't have one") || error.contains("doesn’t have one") {
                                "此磁碟沒有可用的垃圾桶，可改選永久刪除。"
                            } else if error.contains("Permission denied") || error.contains("permission") {
                                "存取權限不足，請確認磁碟與資料夾的刪除權限。"
                            } else { "操作未完成，請查看詳細錯誤。" };
                            ui.label(summary);
                            ui.collapsing(format!("詳細錯誤 · {}", name(entry)), |ui| { ui.label(error); });
                        }
                    });
                }
            });
            if retry {
                if let Some(result) = &self.trash_result {
                    self.pending_trash = result.failed_entries.clone();
                    self.pending_root = Some(result.root.clone());
                    self.permanent = true;
                    self.permanent_ack = false;
                    self.confirm = true;
                    self.show_trash_result = false;
                }
            }
        }
    }
}

/// Binary treemap: split along the longer edge near half of the total weight.
fn partition(
    items: &[(Option<usize>, u64)],
    rect: egui::Rect,
    out: &mut Vec<(Option<usize>, u64, egui::Rect)>,
) {
    if items.len() == 1 {
        out.push((items[0].0, items[0].1, rect));
        return;
    }
    if items.is_empty() {
        return;
    }
    let total: f64 = items.iter().map(|x| x.1 as f64).sum();
    let mut sum = 0.0;
    let mut cut = 1;
    for (i, item) in items.iter().enumerate().take(items.len() - 1) {
        sum += item.1 as f64;
        cut = i + 1;
        if sum >= total / 2.0 {
            break;
        }
    }
    let ratio = (sum / total) as f32;
    let (mut a, mut b) = (rect, rect);
    if rect.width() >= rect.height() {
        a.max.x = rect.min.x + rect.width() * ratio;
        b.min.x = a.max.x;
    } else {
        a.max.y = rect.min.y + rect.height() * ratio;
        b.min.y = a.max.y;
    }
    partition(&items[..cut], a, out);
    partition(&items[cut..], b, out);
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.poll(ctx);
        self.sidebar(ctx);
        self.inspector(ctx);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(249, 250, 248))
                    .inner_margin(24),
            )
            .show(ctx, |ui| {
                ui.add_enabled_ui(self.trash_receiver.is_none(), |ui| self.content(ui, ctx));
            });
        self.dialogs(ctx);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 900.0])
            .with_min_inner_size([1000.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "空間透鏡 · Space Lens",
        options,
        Box::new(|cc| {
            let mut app = App::new(&cc.egui_ctx);
            if let Some(path) = std::env::args_os().nth(1) {
                app.start(PathBuf::from(path), &cc.egui_ctx);
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(ctx: &egui::Context) -> (tempfile::TempDir, App) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("影片資料夾")).unwrap();
        std::fs::write(dir.path().join("影片資料夾/片段.mp4"), [0; 80]).unwrap();
        std::fs::write(dir.path().join("相片.png"), [0; 40]).unwrap();
        std::fs::write(dir.path().join("文件.pdf"), [0; 20]).unwrap();
        let mut app = App::new(ctx);
        let report = scan::scan(dir.path(), &Progress::default()).unwrap();
        app.root = Some(report.entries[0].path.clone());
        app.categories = report
            .entries
            .iter()
            .map(|e| view::classify(&e.path))
            .collect();
        app.category_totals = view::category_totals(&report, &app.categories, 0);
        app.report = Some(report);
        app.rebuild_rows();
        (dir, app)
    }

    #[test]
    fn category_filter_sort_and_batch_snapshot_work_together() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        let report = app.report.as_ref().unwrap();
        assert_eq!(report.entries[app.rows[0]].logical, 80);
        app.selection.ids.extend(app.rows[..2].iter());
        app.request_trash();
        assert_eq!(app.pending_trash.len(), 2);
        app.selection.clear();
        assert_eq!(
            app.pending_trash.len(),
            2,
            "confirmation must keep its snapshot"
        );
        app.confirm = false;
        app.ascending = true;
        app.rebuild_rows();
        assert_eq!(
            app.report.as_ref().unwrap().entries[app.rows[0]].logical,
            20
        );
        app.large_files = true;
        app.category.insert(Category::Video);
        app.rebuild_rows();
        assert_eq!(app.rows.len(), 1);
        assert!(app.report.as_ref().unwrap().entries[app.rows[0]]
            .path
            .ends_with("片段.mp4"));
        app.category.insert(Category::Image);
        app.rebuild_rows();
        assert_eq!(app.rows.len(), 2);
        assert!(app
            .rows
            .iter()
            .all(|&id| !app.report.as_ref().unwrap().entries[id].directory));
        app.selection.ids = app.rows.iter().copied().collect();
        app.request_trash();
        assert!(app.pending_trash.iter().all(|entry| !entry.directory));
    }

    #[test]
    fn layout_keeps_table_and_inspector_inside_minimum_window() {
        let ctx = egui::Context::default();
        let (_dir, mut app) = fixture(&ctx);
        app.selection.ids.insert(app.rows[0]);
        // Exercise the longest category toolbar, independent of fixture distribution.
        app.category_totals = [CategoryTotal {
            logical: 1_500_000_000,
            allocated: 1000,
            count: 3,
        }; 9];
        for window in [Vec2::new(1000.0, 800.0), Vec2::new(1180.0, 900.0)] {
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, window)),
                        ..Default::default()
                    },
                    |ctx| {
                        app.sidebar(ctx);
                        app.inspector(ctx);
                        egui::CentralPanel::default()
                            .frame(egui::Frame::new().inner_margin(24))
                            .show(ctx, |ui| {
                                app.content(ui, ctx);
                                assert!(
                                    ui.min_rect().bottom() <= ui.max_rect().bottom() + 1.0,
                                    "content overflow: {:?} beyond {:?}",
                                    ui.min_rect(),
                                    ui.max_rect()
                                );
                            });
                    },
                );
            }
        }
    }
}
