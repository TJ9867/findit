#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // hide console window on Windows in release

use concurrent_queue::ConcurrentQueue;
use eframe::egui;
use egui::{FontFamily, FontId, IconData, RichText, TextStyle}; // FontFamily, FontId,
use egui_extras::{Column, TableBuilder};
use egui_file_dialog::{DialogState, FileDialog};
use memmap2::Mmap;
use regex::bytes::Regex as BytesRegex;
use regex::Regex as Utf8Regex;
use std::io::{Read, Seek};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

use json;

use work_queue::{LocalQueue, Queue};

use std::collections::VecDeque;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::SeekFrom;
use std::path::Path;
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

fn load_icon() -> IconData {
    let (icon_rgba, icon_width, icon_height) = {
        let icon = include_bytes!("../rsrc/icon/quer-icon-128x128.png");
        let image = image::load_from_memory(icon)
            .expect("Failed to open icon path")
            .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };

    IconData {
        rgba: icon_rgba,
        width: icon_width,
        height: icon_height,
    }
}

fn main() -> Result<(), eframe::Error> {
    env_logger::init(); // Log to stderr (if you run with `RUST_LOG=debug`).

    let viewport_bldr = egui::ViewportBuilder::default().with_icon(load_icon());
    let options = eframe::NativeOptions {
        viewport: viewport_bldr,
        ..Default::default()
    };

    eframe::run_native(
        "quer - Stuff Finder",
        options,
        Box::new(|cc| Ok(Box::new(QuerApp::new(cc)))),
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

#[derive(PartialEq, Clone)]
enum RegexErr {
    InvalidChar,
    EmptyRegex,
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

struct Finding {
    filepath: String,
    offset: usize,
    match_size: usize,
    match_content: String,
}

struct QuerApp {
    regex_str: String,
    filter_str: String,
    root_folder_path: PathBuf,
    export_file_path: PathBuf,
    imhex_file_path: String,
    search_dir_dialog: Option<FileDialog>,
    export_file_dialog: Option<FileDialog>,
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
    clear_results_before_search: bool,
    previous_searches: VecDeque<(String, ContentEnum)>,
    log_lines: Vec<String>,
}

struct SearchOptions {
    alignment: i32,
    regex_result: Result<RegexEnum, String>,
    max_hits: u32,
}

impl Clone for QuerApp {
    fn clone(&self) -> Self {
        Self {
            regex_str: self.regex_str.clone(),
            filter_str: self.filter_str.clone(),
            root_folder_path: self.root_folder_path.clone(),
            export_file_path: self.export_file_path.clone(),
            imhex_file_path: self.imhex_file_path.clone(),
            search_dir_dialog: None,  // this is why we're clonin'
            export_file_dialog: None, // this is why we're clonin'
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
            clear_results_before_search: true,
            previous_searches: VecDeque::new(),
            log_lines: Vec::new(),
        }
    }
}

impl eframe::App for QuerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_main_search_ui(ctx);
    }
}

impl QuerApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_text_styles(&cc);
        Self {
            regex_str: "".to_owned(),
            filter_str: "".to_owned(),
            root_folder_path: PathBuf::from("/"),
            export_file_path: PathBuf::from("/"),
            imhex_file_path: "".to_owned(),
            search_dir_dialog: Option::None,
            export_file_dialog: Option::None,
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
            clear_results_before_search: true,
            previous_searches: VecDeque::new(),
            log_lines: Vec::new(),
        }
    }

    fn add_export_file_dialog(&mut self, ctx: &egui::Context) {
        let mut should_close_dialog = false;
        if let Some(dialog) = &mut self.export_file_dialog {
            let viewport_id = egui::ViewportId::from_hash_of(format!("file_dialog"));
            let viewport_builder = egui::ViewportBuilder::default()
                .with_inner_size((800.0 + 10., 600.0 + 50.))
                .with_resizable(false)
                .with_title(format!("Export File To"))
                .with_decorations(true);

            let viewport_cb = |ctx: &egui::Context, _| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.with_layout(
                        egui::Layout::left_to_right(egui::Align::Center).with_main_justify(true),
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
                self.export_file_path = file.to_path_buf();
                Self::export_findings_to_imhexbm(
                    &self.findings,
                    &self.export_file_path,
                    &self.imhex_file_path,
                );
            }

            match dialog.state() {
                DialogState::Open => {}
                DialogState::Closed => {
                    self.export_file_dialog = None;
                }
                DialogState::Selected(_) => {} // TODO use this in a nicer fashion than rebuilding gui element
                DialogState::SelectedMultiple(_) => {}
                DialogState::Cancelled => {
                    self.export_file_dialog = None;
                }
            }

            if should_close_dialog {
                self.export_file_dialog = None;
            }
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
                self.search_dir_dialog = Some(dialog);
            }

            let mut should_close_dialog = false;
            if let Some(dialog) = &mut self.search_dir_dialog {
                // dialog.update(ctx);
                // if let Some(file) = dialog.take_selected() {
                //     self.root_folder_path = file.to_path_buf();
                //     self.search_dir_dialog = None;
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
                        self.search_dir_dialog = None;
                    }
                    DialogState::Selected(_) => {} // TODO use this in a nicer fashion than rebuilding gui element
                    DialogState::SelectedMultiple(_) => {}
                    DialogState::Cancelled => {
                        self.search_dir_dialog = None;
                    }
                }

                if should_close_dialog {
                    self.search_dir_dialog = None;
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
                let mut unicode_off_re_str = String::from("(?-u)"); // append unicode disable so even non-utf8 stuff matches
                match convert_simplified_hex_regex(&self.regex_str) {
                    Ok(r) => {
                        unicode_off_re_str.push_str(&r);

                        match BytesRegex::new(&unicode_off_re_str) {
                            Ok(unicode_off_re) => {
                                self.regex_result = Ok(RegexEnum::Hex(unicode_off_re));
                            }
                            Err(re_error) => {
                                self.regex_result =
                                    Err(format!("Error compiling regex: {}", re_error).to_string());
                            }
                        }
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
            ui.checkbox(
                &mut self.clear_results_before_search,
                "Clear Results on New Search",
            );
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
                ui.add(
                    egui::widgets::Slider::new(&mut self.max_hits, 1_u32..=2_u32.pow(20))
                        .logarithmic(true),
                );
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

    fn respond_to_match_cell(&mut self, resp: &egui::Response, cell_val: &String) {
        resp.context_menu(|ui| {
            if ui.button(format!("Sort ascending")).clicked() {
                self.findings
                    .sort_by(|a, b| a.match_content.cmp(&b.match_content));
                ui.close_menu();
            }
            if ui.button(format!("Sort descending")).clicked() {
                self.findings
                    .sort_by(|a, b| b.match_content.cmp(&a.match_content));
                ui.close_menu();
            }
            if ui.button(format!("Cancel")).clicked() {
                ui.close_menu();
            }
        });
        resp.clone().on_hover_text(cell_val);
    }

    fn respond_to_filepath_cell(
        &mut self,
        resp: &egui::Response,
        path_value: &String,
        ctx: &egui::Context,
    ) {
        let path = Path::new(path_value);
        let parent = path.parent().unwrap().to_str().unwrap();
        let filename = path.file_name().unwrap().to_str().unwrap();
        resp.context_menu(|ui| {
            if ui.button(format!("Copy full path")).clicked() {
                ctx.copy_text(path_value.to_string());
                ui.close_menu();
            }
            if ui.button(format!("Copy filename")).clicked() {
                ctx.copy_text(filename.to_string());
                ui.close_menu();
            }
            if ui.button(format!("Copy enclosing dir")).clicked() {
                ctx.copy_text(parent.to_string());
                ui.close_menu();
            }
            ui.separator();
            if ui.button(format!("Sort ascending")).clicked() {
                self.findings.sort_by(|a, b| a.filepath.cmp(&b.filepath));
                ui.close_menu();
            }
            if ui.button(format!("Sort descending")).clicked() {
                self.findings.sort_by(|a, b| b.filepath.cmp(&a.filepath));
                ui.close_menu();
            }
            ui.separator();
            if ui
                .button(format!("Export File results to .imhexbm..."))
                .clicked()
            {
                ui.close_menu();

                self.log(format!("Exporting {} to imhexbm", path_value));

                let mut dialog = FileDialog::new()
                    .initial_directory(self.export_file_path.clone())
                    .as_modal(false)
                    .title_bar(false)
                    .movable(false)
                    .resizable(false)
                    .min_size([800., 600.]); //.show_files_filter(filter);
                dialog.save_file();
                self.export_file_dialog = Some(dialog);
                self.imhex_file_path = path_value.clone();
            }

            ui.separator();
            if ui.button("Cancel").clicked() {
                ui.close_menu();
            }
        });

        resp.clone().on_hover_text(format!("{path_value}"));
    }

    fn respond_to_offset_cell(
        &mut self,
        resp: &egui::Response,
        offset: usize,
        ctx: &egui::Context,
    ) {
        let hex_value_w_0x = format!("0x{offset:x}");
        let hex_value = format!("{offset:x}");
        let dec_value = format!("{offset}");
        let hover_text = format!("Hex:\t{hex_value}\nDec:\t{dec_value}");

        resp.context_menu(|ui| {
            if ui
                .button(format!("Copy as hex: {hex_value_w_0x}"))
                .clicked()
            {
                ctx.copy_text(hex_value_w_0x.to_string());
                ui.close_menu();
            }
            if ui.button(format!("Copy as hex: {hex_value}")).clicked() {
                ctx.copy_text(hex_value.to_string());
                ui.close_menu();
            }
            if ui.button(format!("Copy as decimal: {dec_value}")).clicked() {
                ctx.copy_text(dec_value.to_string());
                ui.close_menu();
            }
            ui.separator();
            if ui.button(format!("Sort ascending")).clicked() {
                self.findings.sort_by(|a, b| a.offset.cmp(&b.offset));
                ui.close_menu();
            }
            if ui.button(format!("Sort descending")).clicked() {
                self.findings.sort_by(|a, b| b.offset.cmp(&a.offset));
                ui.close_menu();
            }
            if ui.button("Cancel").clicked() {
                ui.close_menu();
            }
        });

        resp.clone().on_hover_text(hover_text);
    }

    fn bytes_to_hexdump(&self, array: &[u8]) -> String {
        let div_16 = array.len() as f32 / 16_f32;
        let mut hexdump = String::with_capacity(div_16.ceil() as usize * 65);
        let mut ascii_dump = String::with_capacity(16);
        for (pos, byte) in array.iter().enumerate().take(array.len()) {
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

    fn bytes_to_hex(&self, array: &[u8], size: usize) -> String {
        let mut hexdump = String::with_capacity(size * 2);
        for (_pos, byte) in array.iter().enumerate().take(size) {
            hexdump.push_str(&format!("{byte:02X}"));
        }

        return hexdump;
    }

    fn cap_string_length(&self, input: &str, max_length: usize) -> String {
        if max_length == 0 {
            String::new() // Return an empty string if max_length is 0
        } else {
            input.chars().take(max_length).collect()
        }
    }

    fn get_file_contents(
        &self,
        path: &String,
        offset: usize,
        match_length: usize,
    ) -> Option<Vec<u8>> {
        let file_r = File::open(path);
        let mut preview_buff = vec![0; match_length]; // todo make this dynamic and nicer
        match file_r {
            Ok(mut file) => {
                match file.seek(SeekFrom::Start(offset as u64)) {
                    Ok(_) => match file.read(&mut preview_buff) {
                        Ok(_size) => return Some(preview_buff),
                        Err(_e) => {}
                    },
                    Err(_e) => {} // do nothing
                }
            }
            Err(_e) => {} // just dont make gui
        }

        None
    }

    fn build_file_preview(
        &mut self,
        resp: egui::Response,
        path: &String,
        offset: usize,
        match_length: usize,
        ctx: &egui::Context,
    ) {
        resp.context_menu(|ui| {
            if ui.button("Copy as bytes").clicked() {
                let contents = self.get_file_contents(path, offset, match_length).unwrap();
                ctx.copy_text(String::from_utf8_lossy(contents.as_slice()).to_string());

                ui.close_menu();
            }
            if ui.button("Copy as hex bytes").clicked() {
                let contents = self.get_file_contents(path, offset, match_length).unwrap();
                let hex_bytes_str = &mut self.bytes_to_hex(contents.as_slice(), match_length);
                ctx.copy_text(hex_bytes_str.to_string());
                ui.close_menu();
            }
            if ui.button("Copy as hexdump").clicked() {
                let offset = std::cmp::max::<i64>(0 as i64, offset as i64 - 32) as usize;
                let contents = self.get_file_contents(path, offset, 64).unwrap();
                let hex_dump_str = &mut self.bytes_to_hexdump(contents.as_slice());
                ctx.copy_text(hex_dump_str.to_string());
                ui.close_menu();
            }
            if ui.button("Cancel").clicked() {
                ui.close_menu();
            }
        });

        resp.on_hover_ui(|ui| {
            let offset = std::cmp::max::<i64>(0 as i64, offset as i64 - 32) as usize;
            let contents = self.get_file_contents(path, offset, 64).unwrap();

            let hex_dump_str = &mut self.bytes_to_hexdump(contents.as_slice());
            ui.code_editor(hex_dump_str);
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

        ui.separator();

        TableBuilder::new(ui)
            .striped(true)
            .max_scroll_height(f32::INFINITY)
            .sense(egui::Sense {
                click: true,
                drag: true,
                focusable: true,
            })
            .resizable(true)
            .column(Column::remainder().at_least(72.))
            .column(Column::remainder().at_least(64.))
            .column(Column::remainder().at_least(64.))
            .column(Column::remainder())
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.horizontal(|ui| {
                        ui.heading("File Path").on_hover_text(
                            "File path to the file that a given match was found in.",
                        );
                    });
                    ui.separator();
                });
                header.col(|ui| {
                    let resp = ui.heading("Offset");
                    resp.on_hover_text("Offset into the file that the match starts at.");
                    ui.separator();
                });
                header.col(|ui| {
                    ui.heading("Match")
                        .on_hover_text("Contents of the resulting match");
                    ui.separator();
                });
                header.col(|ui| {
                    ui.heading("Preview").on_hover_text("Visualize column");
                    ui.separator();
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
                    self.respond_to_filepath_cell(&resp, &path, ctx);

                    let offset = self.findings[row_index].offset;
                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("0x{offset:x}"))
                            .truncate()
                            .selectable(false);
                        ui.add(label);
                    });
                    self.respond_to_offset_cell(&resp, offset, ctx);

                    let match_content =
                        self.cap_string_length(&self.findings[row_index].match_content, 1000);
                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("{match_content}"))
                            .truncate()
                            .selectable(false);
                        ui.add(label);
                    });

                    self.respond_to_match_cell(&resp, &format!("{match_content}"));

                    let (_rect, resp) = row.col(|ui| {
                        let label = egui::Label::new(format!("🔍")).truncate().selectable(false);
                        ui.add(label);
                    });
                    let match_size = self.findings[row_index].match_size;
                    self.build_file_preview(resp, &path, offset, match_size, ctx);

                    // ^^ this is the click handler
                })
            });
    }

    fn add_regex_line(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("v", |ui| {
                    if self.previous_searches.len() > 0 {
                        for (prev_search, content_type) in self.previous_searches.iter() {
                            if ui.button(prev_search).clicked() {
                                self.regex_str = prev_search.clone();
                                self.content_type = content_type.clone();
                            }
                        }
                        ui.separator();
                        if ui.button("Clear").clicked() {
                            self.previous_searches.clear();
                            ui.close_menu();
                        }
                    } else {
                        ui.label("No previous searches yet");
                    }
                })
                .response
                .on_hover_text("Past searches");
                let regex_edit = egui::TextEdit::singleline(&mut self.regex_str)
                    .hint_text("Enter regex here")
                    .font(TextStyle::Small);

                ui.add_sized(ui.available_size(), regex_edit).on_hover_text(
                    "Examples: abc.ef, ^hello world$, aa{3}h. See mode tooltips for more info.",
                );
            });
        });
    }

    fn add_find_and_clear_btns(&mut self, ui: &mut egui::Ui, _ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let mut btn = egui::Button::new(RichText::new("Search").text_style(TextStyle::Heading));
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

    fn update_main_search_ui(&mut self, ctx: &egui::Context) {
        // Top, search + options
        egui::TopBottomPanel::top("search_options").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("File", |ui| {
                    ui.menu_button("Export", |ui| {
                        if ui.button("Export to CSV...").clicked() {
                            self.log("*clack* (TODO)".to_string());
                            ui.close_menu();
                        }
                    });
                });
                ui.menu_button("About", |ui| {
                    ui.vertical(|ui| {
                        ui.label("quer - A data finding utility");
                        ui.separator();
                        ui.hyperlink_to("Source Code", "https://github.com/TJ9867/quer");
                    });
                });
            });
            self.add_export_file_dialog(ctx);

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
            ui.separator();

            let text_style = TextStyle::Body;
            let row_height = ui.text_style_height(&text_style);

            egui::Frame::none()
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                        ui,
                        row_height,
                        self.log_lines.len(),
                        |ui, row_range| {
                            for row in row_range {
                                ui.label(&self.log_lines[row]);
                            }
                        },
                    )
                })
                .response
                .context_menu(|ui| {
                    if ui.button("Clear Logs").clicked() {
                        self.log_lines.clear();
                    }
                });
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

    fn export_findings_to_imhexbm(
        findings: &Vec<Finding>,
        output_path: &PathBuf,
        filepath: &String,
    ) {
        let mut bookmarks_vec: Vec<json::JsonValue> = Vec::new();
        let mut json_data = json::JsonValue::new_object();

        let mut id: u32 = 1;
        for finding in findings.iter() {
            if *filepath == finding.filepath {
                let mut bookmark_obj = json::JsonValue::new_object();
                bookmark_obj["color"] = 1341756994.into();
                bookmark_obj["comment"] = "\n".into();
                bookmark_obj["id"] = id.into();
                bookmark_obj["locked"] = true.into();
                bookmark_obj["name"] =
                    format!("{} @ 0x{:x}", finding.match_content, finding.offset).into();

                let mut region_obj = json::JsonValue::new_object();
                region_obj["address"] = finding.offset.into();
                region_obj["size"] = finding.match_size.into();
                bookmark_obj["region"] = region_obj;

                bookmarks_vec.push(bookmark_obj);

                id += 1;
            }
        }
        json_data["bookmarks"] = bookmarks_vec.into();

        match fs::write(output_path, json::stringify_pretty(json_data, 4)) {
            Ok(_ok) => {}
            Err(_err) => {}
        }
    }

    fn search(&mut self) {
        if self.clear_results_before_search {
            self.findings.clear();
            self.rx_handles.clear();
        }

        if self.previous_searches.len() == 10 {
            // TODO make configurable
            self.previous_searches.pop_back();
        }

        self.previous_searches
            .push_front((self.regex_str.clone(), self.content_type.clone()));

        self.max_files = 0;
        self.current_files_mtx = Arc::new(Mutex::new(0));

        let filtered_iter = create_filter_iter(
            WalkDir::new(&self.root_folder_path),
            self.file_walk_options.clone(),
        );

        self.log(
            format!(
                "Searching for {} in {}",
                self.regex_str,
                self.root_folder_path.to_str().unwrap()
            )
            .to_string(),
        );

        let count_struct = self.enqueue_files(filtered_iter);

        self.max_files = /*count_struct.num_dirs +*/ count_struct.num_files;
        self.log(format!(
            "Searching {} files, {} directories",
            count_struct.num_dirs, count_struct.num_files
        ));

        if self.max_files < 1 {
            return;
        }

        let (result_tx, result_rx) = mpsc::channel();
        let arc_result_tx = Arc::new(result_tx);

        let (filecount_tx, filecount_rx) = mpsc::channel();
        let arc_filecount_tx = Arc::new(filecount_tx);

        self.rx_handles.push(result_rx);

        self.filecount_handles.push(filecount_rx);

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

    fn log(&mut self, s: String) {
        let date = chrono::Local::now();
        self.log_lines
            .push(format!("{} {}", date.format("[%Y-%m-%d][%H:%M:%S]"), s));
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
        match_size: m.len(),
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
        match_size: m.len(),
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

fn convert_simplified_hex_regex(regex_str: &String) -> Result<String, RegexErr> {
    let no_spaces = regex_str.replace(" ", "");
    let invalid_char_re = Utf8Regex::new("[^a-fA-F0-9.?\\[\\]\\{\\}\\(\\)\\|,-]").unwrap();
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
