#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use concurrent_queue::ConcurrentQueue;
use eframe::egui;
use egui::{FontFamily, FontId, RichText, TextStyle}; // FontFamily, FontId,
use egui_extras::{Column, TableBuilder};
use egui_file_dialog::{DialogState, FileDialog};
use memmap2::Mmap;
use regex::bytes::Regex as BytesRegex;
use regex::Regex as Utf8Regex;
use std::io::{Read, Seek};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use work_queue::{LocalQueue, Queue};

use std::fs::File;
use std::fs::OpenOptions;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::result::Result;
use std::string::String;

use walkdir::{DirEntry, FilterEntry, WalkDir};

struct Task(Box<dyn FnOnce(&mut LocalQueue<Task>) + Send>);

fn expanding_content(ui: &mut egui::Ui) {
    let width = ui.available_width().clamp(20.0, 200.0);
    let height = ui.available_height();
    let (rect, _response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        (1.0, ui.visuals().text_color()),
    );
}

fn configure_text_styles(cc: &eframe::CreationContext<'_>) {
    // Set up font styles so they are little easier to read
    let ctx = &cc.egui_ctx;
    // Get current context style
    let mut style = (*ctx.style()).clone();

    // Redefine text_styles
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(22.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(12.0, FontFamily::Proportional)),
        (
            TextStyle::Monospace,
            FontId::new(11.0, FontFamily::Monospace),
        ),
        (
            TextStyle::Button,
            FontId::new(16.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(16.0, FontFamily::Proportional),
        ),
    ]
    .into();

    // Mutate global style with above changes
    ctx.set_style(style);
}

fn main() -> Result<(), eframe::Error> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default(),
        ..Default::default()
    };
    eframe::run_native(
        "findit - Stuff Finder",
        options,
        Box::new(|cc| Ok(Box::new(FinditApp::new(cc)))),
    )
}

#[derive(PartialEq, Clone)]
enum ContentEnum {
    Hex,
    Text,
}

#[derive(Clone)]
enum RegexEnum {
    Hex(BytesRegex),
    Text(BytesRegex),
}

#[derive(PartialEq, Clone)]
enum FilterTypeEnum {
    AllFiles,
    NoHidden,
}

#[derive(PartialEq, Clone)]
enum LinkBehaviorEnum {
    //    Follow,
    NoFollow,
}

#[derive(Clone)]
struct FileWalkOptions {
    hidden_files: FilterTypeEnum,
    _links: LinkBehaviorEnum,
}

struct FileCount {
    num_files: i32,
    num_dirs: i32,
}

#[derive(PartialEq, Clone)]
enum RegexErr {
    InvalidChar,
    EmptyRegex,
}

struct FinditApp {
    regex_str: String,
    filter_str: String,
    root_folder_path: PathBuf,
    open_file_dialog: Option<FileDialog>,
    content_type: ContentEnum,
    regex_result: Result<RegexEnum, String>,
    file_walk_options: FileWalkOptions,
    progress: f32,
    max_files: i32,
    current_files_mtx: Arc<Mutex<i32>>,
    max_hits: u32,
    file_contents: String,
    alignment: i32,
    worker_threads: Vec<Option<thread::JoinHandle<()>>>,
    findings: Vec<Finding>,
    rx_handles: Vec<mpsc::Receiver<Finding>>,
    filecount_handles: Vec<mpsc::Receiver<i32>>,
    file_queue: Arc<ConcurrentQueue<DirEntry>>,
    work_queue: Option<Queue<Task>>,
}

struct SearchOptions {
    alignment: i32,
    regex_result: Result<RegexEnum, String>,
    max_hits: u32,
}

impl Clone for FinditApp {
    fn clone(&self) -> Self {
        Self {
            regex_str: self.regex_str.clone(),
            filter_str: self.filter_str.clone(),
            root_folder_path: self.root_folder_path.clone(),
            open_file_dialog: None, // this is why we're clonin'
            content_type: self.content_type.clone(),
            regex_result: self.regex_result.clone(),
            file_walk_options: self.file_walk_options.clone(),
            progress: self.progress.clone(),
            max_files: self.max_files.clone(),
            current_files_mtx: self.current_files_mtx.clone(),
            max_hits: self.max_hits.clone(),
            file_contents: self.file_contents.clone(),
            alignment: self.alignment.clone(),
            worker_threads: Vec::new(), // worker threads don't need these vecs
            findings: Vec::new(),
            rx_handles: Vec::new(),
            filecount_handles: Vec::new(),
            file_queue: Arc::new(ConcurrentQueue::unbounded()),
            work_queue: None,
        }
    }
}

impl eframe::App for FinditApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            self.update_main_search_ui(ui, ctx);
        });
    }
}

impl FinditApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_text_styles(&cc);
        Self {
            regex_str: "".to_owned(),
            filter_str: "".to_owned(),
            root_folder_path: PathBuf::from("/"),
            open_file_dialog: Option::None,
            content_type: ContentEnum::Hex,
            regex_result: Ok(RegexEnum::Hex(BytesRegex::new("").unwrap())),
            file_walk_options: FileWalkOptions {
                hidden_files: FilterTypeEnum::NoHidden,
                _links: LinkBehaviorEnum::NoFollow,
            },
            progress: 0.0,
            max_files: 0,
            current_files_mtx: Arc::new(Mutex::new(0)),
            max_hits: 1024 * 1024,
            file_contents: String::from(""),
            alignment: 0,
            worker_threads: Vec::new(),
            findings: Vec::new(),
            rx_handles: Vec::new(),
            filecount_handles: Vec::new(),
            file_queue: Arc::new(ConcurrentQueue::unbounded()),
            work_queue: None,
        }
    }

    fn add_folder_dialog(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let path_label =
                ui.label(RichText::new("Folder to search: ").text_style(TextStyle::Small));
            let mut path_string = String::from(
                self.root_folder_path
                    .to_str()
                    .unwrap_or("Please input path"),
            );

            let path_edit = egui::TextEdit::singleline(&mut path_string).font(TextStyle::Small);
            let path_resp = ui.add_sized(
                [f32::max(ui.available_width() - 60.0, 24.0), 24.0],
                path_edit,
            );
            path_resp.labelled_by(path_label.id);

            if ui.button(RichText::new("Open")).clicked() {
                let mut dialog = FileDialog::new()
                    .initial_directory(self.root_folder_path.clone())
                    .as_modal(false)
                    .title_bar(false)
                    .movable(false)
                    .resizable(false)
                    .min_size([800., 600.]); //.show_files_filter(filter);
                dialog.select_directory();
                self.open_file_dialog = Some(dialog);
            }

            let mut should_close_dialog = false;
            if let Some(dialog) = &mut self.open_file_dialog {
                // dialog.update(ctx);
                // if let Some(file) = dialog.take_selected() {
                //     self.root_folder_path = file.to_path_buf();
                //     self.open_file_dialog = None;
                // }
                let viewport_id = egui::ViewportId::from_hash_of(format!("folder_dialog"));
                let viewport_builder = egui::ViewportBuilder::default()
                    .with_inner_size((800.0 + 10., 600.0 + 50.))
                    .with_resizable(false)
                    .with_title(format!("Open Folder to Search"))
                    .with_decorations(true);

                let viewport_cb = |ctx: &egui::Context, _| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.with_layout(
                            egui::Layout::left_to_right(egui::Align::Center)
                                .with_main_justify(true),
                            |_ui| {
                                dialog.update(ctx);
                            },
                        );
                    });

                    if ctx.input(|i| i.viewport().close_requested()) {
                        should_close_dialog = true;
                    }
                };

                ctx.show_viewport_immediate(viewport_id, viewport_builder, viewport_cb);
                if let Some(file) = dialog.take_selected() {
                    self.root_folder_path = file.to_path_buf();
                }

                match dialog.state() {
                    DialogState::Open => {}
                    DialogState::Closed => {
                        self.open_file_dialog = None;
                    }
                    DialogState::Selected(_) => {} // TODO use this in a nicer fashion than rebuilding gui element
                    DialogState::SelectedMultiple(_) => {}
                    DialogState::Cancelled => {
                        self.open_file_dialog = None;
                    }
                }

                if should_close_dialog {
                    self.open_file_dialog = None;
                }
            }
        });
    }

    fn add_mode_selector(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Mode:").text_style(TextStyle::Small));
            ui.selectable_value(&mut self.content_type, ContentEnum::Hex, "Hex")
                .on_hover_text("Use this mode for data in base 16 pairs. E.g. 'DE AD BE . 00 00'. '.' matches one byte.");
            ui.selectable_value(&mut self.content_type, ContentEnum::Text, "Text")
                .on_hover_text("Use this mode for textual data. E.g. 'Mary had a \\w+ lamb.'");
        });

        // update regex
        match self.content_type {
            ContentEnum::Hex => {
                let mut unicode_off_re = String::from("(?-u)"); // append unicode disable so even non-utf8 stuff matches
                match convert_simplified_hex_regex(&self.regex_str) {
                    Ok(r) => {
                        unicode_off_re.push_str(&r);
                        let re = BytesRegex::new(&unicode_off_re).unwrap();
                        self.regex_result = Ok(RegexEnum::Hex(re));
                    }
                    Err(err) => match err {
                        RegexErr::InvalidChar => {
                            self.regex_result = Err("Invalid char inside hex regex.".to_string())
                        }
                        RegexErr::EmptyRegex => {
                            self.regex_result =
                                Err("Empty regex, please add one to search".to_string())
                        }
                    },
                }
            }
            ContentEnum::Text => {
                if self.regex_str.len() == 0 as usize {
                    self.regex_result = Err("Empty regex, please add one to search".to_string())
                } else {
                    let re = BytesRegex::new(&self.regex_str);

                    match re {
                        Ok(good_re) => {
                            self.regex_result = Ok(RegexEnum::Text(good_re));
                        }
                        Err(re_error) => {
                            self.regex_result =
                                Err(format!("Error compiling regex: {}", re_error).to_string());
                        }
                    }
                }
            }
        }
    }

    fn add_regex_error_line(&mut self, ui: &mut egui::Ui) {
        // add error output if there's something up'
        match &self.regex_result {
            Ok(_good_re) => {} // no need to worry bout this
            Err(err_msg) => {
                ui.horizontal(|ui| {
                    let mut msg = err_msg.to_string();
                    let mut err_msg_te = egui::TextEdit::multiline(&mut msg)
                        .font(TextStyle::Small)
                        .interactive(false)
                        .clip_text(true)
                        .desired_width(0.0)
                        .desired_rows(1);
                    err_msg_te = err_msg_te.text_color(egui::Color32::from_rgb(0x8f, 0x0, 0x0));
                    ui.add_sized([ui.available_width(), 6.0], err_msg_te);
                });
            }
        }
    }

    fn add_advanced_view_options(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("Advanced Search", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    &mut self.file_walk_options.hidden_files,
                    FilterTypeEnum::AllFiles,
                    "All Files",
                );
                ui.selectable_value(
                    &mut self.file_walk_options.hidden_files,
                    FilterTypeEnum::NoHidden,
                    "No Hidden Files",
                );
            });

            ui.horizontal(|ui| {
                let _max_hits_label =
                    ui.label(RichText::new("Max Hits (per File): ").text_style(TextStyle::Small));
                ui.add(egui::widgets::DragValue::new(&mut self.max_hits));
                if self.max_hits < 1 {
                    self.max_hits = 1;
                }
            });
            if self.content_type == ContentEnum::Hex {
                ui.horizontal(|ui| {
                    let _max_hits_label = ui.label(
                        RichText::new("Alignment (0 to disable): ").text_style(TextStyle::Small),
                    );
                    ui.add(egui::widgets::DragValue::new(&mut self.alignment));
                    if self.alignment < 0 {
                        self.alignment = 0;
                    }
                });
            }
        });
    }

    fn respond_to_cell(&mut self, resp: &egui::Response, cell_val: &String, ctx: &egui::Context) {
        if resp.clicked() {
            ctx.copy_text(cell_val.to_string());
        }

        resp.clone().on_hover_text(cell_val);
    }

    fn respond_to_offset_cell(
        &mut self,
        resp: &egui::Response,
        offset: usize,
        ctx: &egui::Context,
    ) {
        let hex_value_copy = format!("0x{offset:x}");
        let hex_value = format!("{offset:x}");
        let dec_value = format!("{offset}");
        let hover_text = format!("Hex:\t{hex_value}\nDec:\t{dec_value}");
        if resp.clicked() {
            ctx.copy_text(hex_value_copy.to_string());
        }

        resp.clone().on_hover_text(hover_text);
    }

    fn bytes_to_hexdump(&self, array: &[u8], size: usize) -> String {
        let div_16 = array.len() as f32 / 16_f32;
        let mut hexdump = String::with_capacity(div_16.ceil() as usize * 65);
        let mut ascii_dump = String::with_capacity(16);
        for (pos, byte) in array.iter().enumerate().take(size) {
            hexdump.push_str(&format!("{byte:02X} "));

            if byte.is_ascii() && !byte.is_ascii_whitespace() && !byte.eq(&0) {
                ascii_dump.push(*byte as char)
            } else {
                ascii_dump.push('.');
            }

            if pos % 8 == 7 {
                hexdump.push_str("    ");
                hexdump.push_str(&ascii_dump);
                ascii_dump.clear();
                hexdump.push('\n');
            }
        }

        return hexdump;
    }

    fn build_file_preview(
        &mut self,
        resp: egui::Response,
        path: &String,
        offset: usize,
        ctx: &egui::Context,
    ) {
        if resp.clicked() {
            ctx.copy_text("yeet".to_string());
        }
        resp.on_hover_ui(|ui| {
            let file_r = File::open(path);
            let mut preview_buff: [u8; 64] = [0; 64]; // todo make this dynamic and nicer
            match file_r {
                Ok(mut file) => {
                    let start_offset = std::cmp::max::<i64>(0 as i64, offset as i64 - 32);
                    match file.seek(SeekFrom::Start(start_offset as u64)) {
                        Ok(_) => match file.read(&mut preview_buff) {
                            Ok(size) => {
                                let hex_str = &mut self.bytes_to_hexdump(&mut preview_buff, size);
                                ui.code_editor(hex_str);
                            }
                            Err(e) => {
                                ui.code_editor(&mut e.to_string());
                            }
                        },
                        Err(e) => {
                            ui.code_editor(&mut e.to_string());
                        } // do nothing
                    }
                }
                Err(_err_msg) => {} // just dont make gui
            }
        });
    }

    fn add_listing_and_content_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        for rx in self.rx_handles.iter() {
            for item in rx.try_iter() {
                self.findings.push(item);
            }
        }

        for rx in self.filecount_handles.iter() {
            for val in rx.try_iter() {
                let mut num = self.current_files_mtx.lock().unwrap();
                *num += val;
            }
        }

        TableBuilder::new(ui)
            .striped(true)
            .auto_shrink(true)
            .sense(egui::Sense {
                click: true,
                drag: true,
                focusable: true,
            })
            .resizable(true)
            .column(Column::remainder().at_least(72.))
            .column(Column::remainder().at_least(64.))
            .column(Column::remainder().at_least(64.))
            .column(Column::remainder().at_least(8.).at_most(100.))
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.heading("File Path")
                        .on_hover_text("File path to the file that a given match was found in.");
                });
                header.col(|ui| {
                    ui.heading("Offset")
                        .on_hover_text("Offset into the file that the match starts at.");
                });
                header.col(|ui| {
                    ui.heading("Match")
                        .on_hover_text("Contents of the resulting match");
                });
                header.col(|ui| {
                    ui.heading("Preview").on_hover_text("Visualize column");
                });
            })
            .body(|body| {
                let row_height = 22.0;
                let num_rows = std::cmp::min(self.findings.len(), 10_000_000);
                body.rows(row_height, num_rows, |mut row| {
                    let row_index = row.index();

                    let path = &self.findings[row_index].filepath.clone();
                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("{path}"))
                            .truncate()
                            .selectable(false);
                        ui.add(label);
                        expanding_content(ui);
                    });
                    self.respond_to_cell(&resp, &format!("{path}"), ctx);

                    let offset = self.findings[row_index].offset;
                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("0x{offset:x}"))
                            .truncate()
                            .selectable(false);
                        ui.add(label);
                    });
                    self.respond_to_offset_cell(&resp, offset, ctx);

                    let match_content = &self.findings[row_index].match_content;
                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("{match_content}"))
                            .truncate()
                            .selectable(false);
                        ui.add(label);
                    });

                    self.respond_to_cell(&resp, &format!("{match_content}"), ctx);

                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("🔍")).truncate().selectable(false);
                        ui.add(label);
                    });
                    self.build_file_preview(resp, &path, offset, ctx);

                    // ^^ this is the click handler
                })
            });
    }

    fn add_regex_line(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let regex_edit = egui::TextEdit::singleline(&mut self.regex_str)
                .hint_text("Enter regex here")
                .font(TextStyle::Small);
            let regex_resp = ui
                .add_sized([ui.available_width(), 12.0], regex_edit)
                .highlight();
            regex_resp.on_hover_text(
                "Examples: abc.ef, ^hello world$, aa{3}h. See mode tooltips for more info.",
            );
        });
    }

    fn add_find_and_clear_btns(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let mut btn = egui::Button::new(RichText::new("FindIt").text_style(TextStyle::Heading));
            let enable_btn;
            let is_find_btn;
            match &self.regex_result {
                Ok(_good_re) => match self.current_files_mtx.lock() {
                    Ok(curr_files) => {
                        if curr_files.eq(&self.max_files) || self.max_files == 0 {
                            btn = btn.fill(egui::Color32::from_rgb(0x2a, 0x7e, 0x43));
                            enable_btn = self.is_search_finished();
                            is_find_btn = true;
                        } else {
                            btn = egui::Button::new(
                                RichText::new("Stop").text_style(TextStyle::Heading),
                            );
                            btn = btn.fill(egui::Color32::from_rgb(0x8f, 0x00, 0x00));
                            enable_btn = true;
                            is_find_btn = false;
                        }
                    }
                    Err(_) => {
                        println!("Error locking current files");
                        btn = btn.fill(egui::Color32::from_rgb(0x3f, 0x3f, 0x3f));
                        enable_btn = false;
                        is_find_btn = false;
                    }
                }, // no need to worry bout this
                Err(_err_msg) => {
                    btn = btn.fill(egui::Color32::from_rgb(0x3f, 0x3f, 0x3f));
                    enable_btn = false;
                    is_find_btn = false;
                }
            }

            if ui.add_enabled(enable_btn, btn).clicked() {
                if is_find_btn {
                    self.progress = 0.0;
                    self.search();
                } else {
                    self.progress = 0.0;

                    // empty the queue
                    while !self.file_queue.is_empty() {
                        self.file_queue.pop().unwrap();
                    }

                    self.rx_handles.clear(); // drop the rx handles so the threads wont write

                    self.max_files = 0;
                }
            }

            if self.findings.len() > 0 {
                let btn = egui::Button::new(
                    RichText::new("Clear Results").text_style(TextStyle::Heading),
                );
                // let btn = btn.fill(egui::Color32::from_rgb(0xf, 0x3f, 0x3f));
                if ui.add_enabled(self.is_search_finished(), btn).clicked() {
                    self.findings.clear();
                    self.rx_handles.clear();
                }
            }
        });
    }

    fn add_search_desc(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        let _regex_label = ui.label(
            RichText::new("Searching for: ".to_owned() + &self.regex_str)
                .text_style(TextStyle::Small),
        );
        if self.findings.len() > 0 {
            let _findings_label =
                ui.label(format!("Found {} results.", self.findings.len()).to_owned());
        }
        ui.horizontal(|ui| {
            if let Ok(count) = self.current_files_mtx.lock() {
                if self.max_files > 0 {
                    self.progress = *count as f32 / self.max_files as f32;
                }
            }
            if !self.is_search_finished() {
                ui.spinner();
            } else if self.worker_threads.len() > 0 {
                self.cleanup_threads();
            }
        });
    }

    fn add_filter_line(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let filter_edit = egui::TextEdit::singleline(&mut self.filter_str)
                .hint_text(RichText::new("Filter...").text_style(TextStyle::Small));
            let filter_resp = ui
                .add_sized([ui.available_width(), 12.0], filter_edit)
                .highlight();
            filter_resp.on_hover_text("Filter results by string value, offset or preview text");
        });
    }

    fn update_main_search_ui(&mut self, _ui: &mut egui::Ui, ctx: &egui::Context) {
        // ui.with_layout(
        //     egui::Layout::centered_and_justified(egui::Direction::TopDown),
        //     |ui| {
        //         egui::Grid::new("search_ui_id").show(ui, |ui| {
        //
        //         });
        //     },
        // );

        // Top, search + options
        egui::TopBottomPanel::top("search_options").show(ctx, |ui| {
            self.add_regex_line(ui, ctx);
            self.add_regex_error_line(ui);
            self.add_folder_dialog(ui, ctx);
            self.add_mode_selector(ui);
            self.add_advanced_view_options(ui);
            ui.end_row();
        });

        // Bottom, progress etc
        egui::TopBottomPanel::bottom("search_progress").show(ctx, |ui| {
            let progress = egui::widgets::ProgressBar::new(self.progress);
            ui.add(progress);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            self.add_find_and_clear_btns(ui, ctx);

            self.add_search_desc(ui, ctx);

            self.add_filter_line(ui, ctx);

            self.add_listing_and_content_view(ui, ctx);
        });
    }

    fn get_search_options(&self) -> SearchOptions {
        SearchOptions {
            alignment: self.alignment,
            regex_result: self.regex_result.clone(),
            max_hits: self.max_hits,
        }
    }

    fn enqueue_files<P: core::ops::FnMut(&DirEntry) -> bool>(
        &mut self,
        file_iter: FilterEntry<walkdir::IntoIter, P>,
    ) -> FileCount {
        let mut file_count = 0;
        let mut dir_count = 0;

        for entry in file_iter {
            if entry.is_ok() {
                if let Some(ent) = entry.as_ref().ok() {
                    if ent.file_type().is_file() {
                        file_count += 1;
                        self.file_queue.push(ent.clone()).unwrap();
                    } else if ent.file_type().is_dir() {
                        dir_count += 1;
                    }
                }
            }
        }

        FileCount {
            num_files: file_count,
            num_dirs: dir_count,
        }
    }

    fn search(&mut self) {
        self.max_files = 0;
        self.current_files_mtx = Arc::new(Mutex::new(0));

        let filtered_iter = create_filter_iter(
            WalkDir::new(&self.root_folder_path),
            self.file_walk_options.clone(),
        );

        println!(
            "Searching for {} in {}",
            self.regex_str,
            self.root_folder_path.to_str().unwrap()
        );

        let count_struct = self.enqueue_files(filtered_iter);

        self.max_files = /*count_struct.num_dirs +*/ count_struct.num_files;
        println!(
            "Searching {} files, {} directories",
            count_struct.num_dirs, count_struct.num_files
        );

        if self.max_files < 1 {
            return;
        }

        let (result_tx, result_rx) = mpsc::channel();
        let arc_result_tx = Arc::new(result_tx);

        let (filecount_tx, filecount_rx) = mpsc::channel();
        let arc_filecount_tx = Arc::new(filecount_tx);

        self.rx_handles.push(result_rx);

        self.filecount_handles.push(filecount_rx);

        let filtered_iter = create_filter_iter(
            WalkDir::new(&self.root_folder_path),
            self.file_walk_options.clone(),
        );
        let search_opts = Arc::new(self.get_search_options());

        let search_threads = 10;

        let queue: Queue<Task> = Queue::new(search_threads, 4096);

        for _i in 0..count_struct.num_files {
            let search_opts_ref = Arc::clone(&search_opts);
            let file_entry_q = Arc::clone(&self.file_queue);
            let result_tx = Arc::clone(&arc_result_tx);
            let filecount_tx = Arc::clone(&arc_filecount_tx);
            queue.push(Task(Box::new(move |_local| {
                if let Ok(filt_ent) = file_entry_q.pop() {
                    search_file(&filt_ent, &result_tx, search_opts_ref);
                }

                match filecount_tx.send(1) {
                    Ok(_) => {}
                    Err(_err) => {
                        //println!("Error sending result {:?}", err);
                    }
                }
            })));
        }

        let thread_handles: Vec<_> = queue
            .local_queues()
            .map(|mut local_queue| {
                std::thread::spawn(move || {
                    while let Some(task) = local_queue.pop() {
                        task.0(&mut local_queue);
                    }
                })
            })
            .collect();

        for handle in thread_handles {
            self.worker_threads.push(Some(handle));
        }

        self.work_queue = Some(queue);
    }

    fn is_search_finished(&self) -> bool {
        for thread in self.worker_threads.iter() {
            if let Some(thread_ref) = thread.as_ref() {
                if !thread_ref.is_finished() {
                    return false;
                }
            }
        }
        true
    }

    fn cleanup_threads(&mut self) {
        for mut thread in self.worker_threads.drain(..) {
            if let Some(thread_taken) = thread.take() {
                match thread_taken.join() {
                    Ok(_) => {}
                    Err(err) => {
                        println!("Error joining on thread {:#?}", err)
                    }
                }
            }
        }
    }
}

fn search_file(entry: &DirEntry, tx: &mpsc::Sender<Finding>, search_opts: Arc<SearchOptions>) {
    let f_res = OpenOptions::new().read(true).open(entry.path());

    if let Ok(f) = f_res {
        let file_data = unsafe {
            // this is marked as unsafe because the contents of the backing file can change
            // outside of the compiler's expectation (and thus contents of refs may change etc)
            Mmap::map(&f)
        };

        if !file_data.is_ok() {
            return;
        }

        let mut curr_hits = 0;

        match search_opts.regex_result.clone() {
            Ok(re_enum) => match &re_enum {
                RegexEnum::Hex(hex_re) => {
                    for m in hex_re.find_iter(&file_data.unwrap()[..]) {
                        process_binary_match(&search_opts, m, &entry, &tx);
                        curr_hits += 1;
                        if curr_hits >= search_opts.max_hits {
                            return;
                        }
                    }
                }
                RegexEnum::Text(txt_re) => {
                    for m in txt_re.find_iter(&file_data.unwrap()[..]) {
                        process_text_match(&search_opts, m, &entry, &tx);

                        curr_hits += 1;
                        if curr_hits >= search_opts.max_hits {
                            return;
                        }
                    }
                }
            },

            Err(_err_msg) => {
                return; // don't continue if there's a problem with regex
            }
        }
    }
}

fn process_binary_match(
    search_opts: &SearchOptions,
    m: regex::bytes::Match,
    entry: &DirEntry,
    tx: &mpsc::Sender<Finding>,
) {
    if search_opts.alignment != 0 && (m.start() % search_opts.alignment as usize) != 0 {
        return;
    }
    match tx.send(Finding {
        filepath: String::from(entry.path().to_str().unwrap()),
        offset: m.start(),
        match_content: m
            .as_bytes()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" "),
    }) {
        Ok(_) => {}
        Err(_) => { /* TODO do something about an error */ }
    }
}

fn process_text_match(
    _search_opts: &SearchOptions,
    m: regex::bytes::Match,
    entry: &DirEntry,
    tx: &mpsc::Sender<Finding>,
) {
    match tx.send(Finding {
        filepath: String::from(entry.path().to_str().unwrap()),
        offset: m.start(),
        match_content: String::from_utf8_lossy(m.as_bytes()).to_string(),
    }) {
        Ok(_) => {}
        Err(_) => { /* TODO do something about an error */ }
    }
}

fn create_filter_iter(
    wlkdir: WalkDir,
    options: FileWalkOptions,
) -> FilterEntry<walkdir::IntoIter, impl core::ops::FnMut(&DirEntry) -> bool> {
    return wlkdir
        .into_iter()
        .filter_entry(move |e| match options.hidden_files {
            FilterTypeEnum::NoHidden => {
                return !is_hidden(e);
            }
            FilterTypeEnum::AllFiles => {
                return true;
            }
        });
}

struct Finding {
    filepath: String,
    offset: usize,
    match_content: String,
}

fn convert_simplified_hex_regex(regex_str: &String) -> Result<String, RegexErr> {
    let no_spaces = regex_str.replace(" ", "");
    let invalid_char_re = Utf8Regex::new("[^a-fA-F0-9.?]").unwrap();
    if let Some(_) = invalid_char_re.find(&no_spaces) {
        // found an invalid character
        return Err(RegexErr::InvalidChar);
    }
    if regex_str.is_empty() {
        return Err(RegexErr::EmptyRegex);
    }
    let hex_bytes_re = Utf8Regex::new("([a-fA-F0-9]{2})").unwrap();
    let add_x_escapes = hex_bytes_re.replace_all(&no_spaces, "\\x$1");

    Ok(add_x_escapes.to_string())
}

// identify unix hidden files
fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with("."))
        .unwrap_or(false)
}
