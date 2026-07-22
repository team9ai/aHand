use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use ahand_protocol::{FileError, FileErrorCode};
use calamine::{Data, Range, Reader, SheetType, SheetVisible, open_workbook_auto};

use super::file_error;

const DEFAULT_MAX_CHARS: usize = 12_000;
const MAX_SAMPLE_REGIONS: usize = 6;
const MAX_SAMPLE_CELLS_PER_REGION: usize = 36;
const MAX_IMPORTANT_SAMPLE_CELLS_PER_REGION: usize = 24;
const SAMPLE_TEXT_LIMIT: usize = 120;
const MAX_FORMULA_RECORDS: usize = 200;

#[derive(Clone)]
struct SheetPreview {
    name: String,
    index: usize,
    visible: bool,
    hidden: bool,
    used_range: String,
    top_left_used_cell: String,
    row_count: u32,
    column_count: u32,
    cells: Vec<PreviewCell>,
    formula_count: usize,
}

#[derive(Clone)]
struct PreviewCell {
    row: u32,
    col: u32,
    value: Option<Data>,
    formula: Option<String>,
}

struct Region {
    range: String,
    cells: Vec<PreviewCell>,
    min_row: u32,
    max_row: u32,
    min_col: u32,
    max_col: u32,
}

struct Block {
    label: String,
    content: String,
    truncated: bool,
}

enum JsonValue {
    String(String),
    Bool(bool),
    U64(u64),
    I64(i64),
    F64(f64),
    Null,
}

pub fn is_spreadsheet_path(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("xls" | "xlsx")
    )
}

pub fn render_spreadsheet_preview(resolved: &Path, req_path: &str) -> Result<String, FileError> {
    let mut workbook = open_workbook_auto(resolved).map_err(|err| {
        file_error(
            FileErrorCode::Encoding,
            req_path,
            format!("failed to open spreadsheet: {err}"),
        )
    })?;
    let file_name = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(req_path);

    let metadata = workbook.sheets_metadata().to_vec();
    let mut sheets = Vec::new();
    for (index, sheet) in metadata.iter().enumerate() {
        if sheet.typ != SheetType::WorkSheet {
            continue;
        }
        let range = workbook.worksheet_range(&sheet.name).map_err(|err| {
            file_error(
                FileErrorCode::Encoding,
                req_path,
                format!("failed to read sheet '{}': {err}", sheet.name),
            )
        })?;
        let formulas = workbook.worksheet_formula(&sheet.name).ok();
        sheets.push(build_sheet_preview(index, sheet, &range, formulas.as_ref()));
    }

    let selected_indices = sheets
        .iter()
        .enumerate()
        .filter_map(|(idx, sheet)| sheet.visible.then_some(idx))
        .collect::<Vec<_>>();
    let blocks = build_blocks(file_name, &sheets, &selected_indices);
    Ok(compose_llm_preview(&blocks, DEFAULT_MAX_CHARS).0)
}

fn build_sheet_preview(
    index: usize,
    sheet: &calamine::Sheet,
    range: &Range<Data>,
    formulas: Option<&Range<String>>,
) -> SheetPreview {
    let mut cells_by_pos: BTreeMap<(u32, u32), PreviewCell> = BTreeMap::new();
    let (range_row_start, range_col_start) = range.start().unwrap_or((0, 0));

    for (row, col, value) in range.used_cells() {
        let abs_row = range_row_start + row as u32 + 1;
        let abs_col = range_col_start + col as u32 + 1;
        cells_by_pos.insert(
            (abs_row, abs_col),
            PreviewCell {
                row: abs_row,
                col: abs_col,
                value: Some(value.clone()),
                formula: None,
            },
        );
    }

    if let Some(formula_range) = formulas {
        let (formula_row_start, formula_col_start) = formula_range.start().unwrap_or((0, 0));
        for (row, col, formula) in formula_range.used_cells() {
            if formula.is_empty() {
                continue;
            }
            let abs_row = formula_row_start + row as u32 + 1;
            let abs_col = formula_col_start + col as u32 + 1;
            let entry = cells_by_pos
                .entry((abs_row, abs_col))
                .or_insert(PreviewCell {
                    row: abs_row,
                    col: abs_col,
                    value: None,
                    formula: None,
                });
            entry.formula = Some(normalize_formula(formula));
        }
    }

    let cells = cells_by_pos.into_values().collect::<Vec<_>>();
    let (used_range, top_left, row_count, column_count) = sheet_bounds(&cells);
    let visible = sheet.visible == SheetVisible::Visible;

    SheetPreview {
        name: sheet.name.clone(),
        index,
        visible,
        hidden: !visible,
        used_range,
        top_left_used_cell: top_left,
        row_count,
        column_count,
        formula_count: cells.iter().filter(|cell| cell.formula.is_some()).count(),
        cells,
    }
}

fn build_blocks(
    file_name: &str,
    sheets: &[SheetPreview],
    selected_indices: &[usize],
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let selected_names = selected_indices
        .iter()
        .map(|idx| sheets[*idx].name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let hidden_count = sheets.iter().filter(|sheet| sheet.hidden).count();

    blocks.push(markdown_block(
        "overview",
        [
            format!("Workbook: {file_name}"),
            format!("Sheets: {} total, {hidden_count} hidden", sheets.len()),
            format!("Previewed sheets: {selected_names}"),
            "Structured format: ndjson".to_string(),
            format!("Budget: max_chars={DEFAULT_MAX_CHARS}"),
        ]
        .join("\n"),
    ));

    blocks.push(structured_block(
        "inventory",
        None,
        inventory_records(file_name, sheets),
    ));

    let notes = notes_block(sheets, selected_indices);
    if !notes.is_empty() {
        blocks.push(markdown_block("notes", notes));
    }

    for idx in selected_indices {
        let sheet = &sheets[*idx];
        let regions = detect_regions(&sheet.cells);
        blocks.push(structured_block(
            "sheet_info",
            Some(&sheet.name),
            vec![sheet_info_record(sheet)],
        ));

        let signals = signal_records(sheet);
        if !signals.is_empty() {
            blocks.push(structured_block("signals", Some(&sheet.name), signals));
        }

        let structure = structure_records(sheet, &regions);
        if !structure.is_empty() {
            blocks.push(structured_block("structure", Some(&sheet.name), structure));
        }

        let samples = sample_records(sheet, &regions);
        if !samples.is_empty() {
            blocks.push(structured_block("samples", Some(&sheet.name), samples));
        }

        let formulas = formula_records(sheet);
        if !formulas.is_empty() {
            blocks.push(structured_block("formulas", Some(&sheet.name), formulas));
        }
    }

    blocks
}

fn inventory_records(file_name: &str, sheets: &[SheetPreview]) -> Vec<String> {
    let hidden_count = sheets.iter().filter(|sheet| sheet.hidden).count();
    let mut records = vec![json_record(vec![
        ("part", JsonValue::String("inventory".to_string())),
        ("kind", JsonValue::String("workbook".to_string())),
        ("fileName", JsonValue::String(file_name.to_string())),
        ("sheetCount", JsonValue::U64(sheets.len() as u64)),
        ("hiddenSheetCount", JsonValue::U64(hidden_count as u64)),
    ])];

    for sheet in sheets {
        records.push(json_record(vec![
            ("part", JsonValue::String("inventory".to_string())),
            ("kind", JsonValue::String("sheet".to_string())),
            ("id", JsonValue::String(format!("ws/{}", sheet.index + 1))),
            ("index", JsonValue::U64(sheet.index as u64)),
            ("name", JsonValue::String(sheet.name.clone())),
            ("visible", JsonValue::Bool(sheet.visible)),
            ("hidden", JsonValue::Bool(sheet.hidden)),
            ("usedRange", JsonValue::String(sheet.used_range.clone())),
            ("rowCount", JsonValue::U64(sheet.row_count as u64)),
            ("columnCount", JsonValue::U64(sheet.column_count as u64)),
        ]));
    }

    records
}

fn notes_block(sheets: &[SheetPreview], selected_indices: &[usize]) -> String {
    let selected = selected_indices.iter().copied().collect::<HashSet<_>>();
    let mut lines = Vec::new();
    for sheet in sheets {
        if sheet.hidden {
            let sampled = selected.contains(&sheet.index);
            let message = if sampled {
                format!("Hidden sheet '{}' was explicitly sampled.", sheet.name)
            } else {
                format!(
                    "Hidden sheet '{}' was detected but not sampled.",
                    sheet.name
                )
            };
            lines.push(format!("- {message}"));
        }
    }
    for idx in selected_indices {
        let sheet = &sheets[*idx];
        if sheet.formula_count > 0 {
            lines.push(format!(
                "- Formula cached values may be unavailable in sheet '{}'; formula text is included.",
                sheet.name
            ));
        }
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!("Notes:\n{}", lines.join("\n"))
    }
}

fn sheet_info_record(sheet: &SheetPreview) -> String {
    json_record(vec![
        ("part", JsonValue::String("sheet_info".to_string())),
        ("sheet", JsonValue::String(sheet.name.clone())),
        ("id", JsonValue::String(format!("ws/{}", sheet.index + 1))),
        ("index", JsonValue::U64(sheet.index as u64)),
        ("visible", JsonValue::Bool(sheet.visible)),
        ("hidden", JsonValue::Bool(sheet.hidden)),
        ("protected", JsonValue::Bool(false)),
        ("usedRange", JsonValue::String(sheet.used_range.clone())),
        (
            "topLeftUsedCell",
            JsonValue::String(sheet.top_left_used_cell.clone()),
        ),
        ("empty", JsonValue::Bool(sheet.cells.is_empty())),
        ("rowCount", JsonValue::U64(sheet.row_count as u64)),
        ("columnCount", JsonValue::U64(sheet.column_count as u64)),
    ])
}

fn signal_records(sheet: &SheetPreview) -> Vec<String> {
    let mut records = Vec::new();
    if sheet.cells.is_empty() {
        records.push(json_record(vec![
            ("part", JsonValue::String("signals".to_string())),
            ("sheet", JsonValue::String(sheet.name.clone())),
            ("signal", JsonValue::String("empty".to_string())),
            ("confidence", JsonValue::String("high".to_string())),
            (
                "reason",
                JsonValue::String("no non-empty cells found in used range".to_string()),
            ),
        ]));
        return records;
    }

    if sheet.top_left_used_cell != "A1" {
        records.push(json_record(vec![
            ("part", JsonValue::String("signals".to_string())),
            ("sheet", JsonValue::String(sheet.name.clone())),
            (
                "signal",
                JsonValue::String("starts_away_from_a1".to_string()),
            ),
            (
                "topLeftUsedCell",
                JsonValue::String(sheet.top_left_used_cell.clone()),
            ),
            (
                "reason",
                JsonValue::String(format!("first used cell is {}", sheet.top_left_used_cell)),
            ),
        ]));
    }

    if sheet.formula_count > 0 {
        records.push(json_record(vec![
            ("part", JsonValue::String("signals".to_string())),
            ("sheet", JsonValue::String(sheet.name.clone())),
            ("signal", JsonValue::String("has_formulas".to_string())),
            (
                "formulaCellCount",
                JsonValue::U64(sheet.formula_count as u64),
            ),
            (
                "reason",
                JsonValue::String("formula cells detected in used range".to_string()),
            ),
        ]));
    }

    if looks_like_single_table(sheet) {
        records.push(json_record(vec![
            ("part", JsonValue::String("signals".to_string())),
            ("sheet", JsonValue::String(sheet.name.clone())),
            ("signal", JsonValue::String("likely_data_sheet".to_string())),
            ("confidence", JsonValue::String("medium".to_string())),
            (
                "reason",
                JsonValue::String(
                    "used range is a compact rectangle with multiple populated rows".to_string(),
                ),
            ),
        ]));
    }

    records
}

fn structure_records(sheet: &SheetPreview, regions: &[Region]) -> Vec<String> {
    regions
        .iter()
        .map(|region| {
            json_record(vec![
                ("part", JsonValue::String("structure".to_string())),
                ("sheet", JsonValue::String(sheet.name.clone())),
                ("kind", JsonValue::String("region".to_string())),
                ("range", JsonValue::String(region.range.clone())),
                ("role", JsonValue::String(region_role(region).to_string())),
            ])
        })
        .collect()
}

fn sample_records(sheet: &SheetPreview, regions: &[Region]) -> Vec<String> {
    let mut records = Vec::new();
    let mut seen = HashSet::new();
    for region in regions.iter().take(MAX_SAMPLE_REGIONS) {
        records.push(json_record(vec![
            ("part", JsonValue::String("samples".to_string())),
            ("sheet", JsonValue::String(sheet.name.clone())),
            ("kind", JsonValue::String("sample".to_string())),
            ("range", JsonValue::String(region.range.clone())),
            ("role", JsonValue::String(region_role(region).to_string())),
        ]));

        for cell in region.cells.iter().take(MAX_SAMPLE_CELLS_PER_REGION) {
            append_cell_sample(&mut records, &mut seen, sheet, cell, &region.range);
        }

        let mut important_added = 0;
        for cell in &region.cells {
            if cell.formula.is_some() || is_long_text_cell(cell) {
                if append_cell_sample(&mut records, &mut seen, sheet, cell, &region.range) {
                    important_added += 1;
                }
                if important_added >= MAX_IMPORTANT_SAMPLE_CELLS_PER_REGION {
                    break;
                }
            }
        }
    }
    records
}

fn formula_records(sheet: &SheetPreview) -> Vec<String> {
    sheet
        .cells
        .iter()
        .filter(|cell| cell.formula.is_some())
        .take(MAX_FORMULA_RECORDS)
        .map(|cell| {
            let mut fields = vec![
                ("part", JsonValue::String("formulas".to_string())),
                ("sheet", JsonValue::String(sheet.name.clone())),
                ("kind", JsonValue::String("cell".to_string())),
                (
                    "address",
                    JsonValue::String(qualified_cell_address(&sheet.name, cell.row, cell.col)),
                ),
                (
                    "formula",
                    JsonValue::String(cell.formula.clone().unwrap_or_default()),
                ),
            ];
            if let Some(value) = cell_json_value(cell) {
                fields.push(("value", value));
            } else {
                fields.push(("cachedValueUnavailable", JsonValue::Bool(true)));
            }
            json_record(fields)
        })
        .collect()
}

fn append_cell_sample(
    records: &mut Vec<String>,
    seen: &mut HashSet<(String, u32, u32)>,
    sheet: &SheetPreview,
    cell: &PreviewCell,
    source_range: &str,
) -> bool {
    let key = (source_range.to_string(), cell.row, cell.col);
    if !seen.insert(key) {
        return false;
    }

    let mut fields = vec![
        ("part", JsonValue::String("samples".to_string())),
        ("sheet", JsonValue::String(sheet.name.clone())),
        ("kind", JsonValue::String("cell".to_string())),
        (
            "address",
            JsonValue::String(qualified_cell_address(&sheet.name, cell.row, cell.col)),
        ),
        ("sourceRange", JsonValue::String(source_range.to_string())),
    ];

    if let Some(value) = cell_json_value(cell) {
        fields.push(("value", value));
    } else {
        fields.push(("value", JsonValue::Null));
    }
    if let Some(formula) = &cell.formula {
        fields.push(("formula", JsonValue::String(formula.clone())));
        if cell_json_value(cell).is_none() {
            fields.push(("cachedValueUnavailable", JsonValue::Bool(true)));
        }
    }

    records.push(json_record(fields));
    true
}

fn detect_regions(cells: &[PreviewCell]) -> Vec<Region> {
    let mut remaining = cells
        .iter()
        .cloned()
        .map(|cell| ((cell.row, cell.col), cell))
        .collect::<BTreeMap<_, _>>();
    let mut regions = Vec::new();

    while let Some((&coordinate, first_cell)) = remaining.iter().next() {
        let first_cell = first_cell.clone();
        remaining.remove(&coordinate);
        let mut stack = vec![coordinate];
        let mut component = vec![first_cell];
        let mut min_row = coordinate.0;
        let mut max_row = coordinate.0;
        let mut min_col = coordinate.1;
        let mut max_col = coordinate.1;

        while let Some((row, col)) = stack.pop() {
            for neighbor in [
                (row.saturating_sub(1), col),
                (row + 1, col),
                (row, col.saturating_sub(1)),
                (row, col + 1),
            ] {
                if neighbor.0 == 0 || neighbor.1 == 0 {
                    continue;
                }
                let Some(cell) = remaining.remove(&neighbor) else {
                    continue;
                };
                stack.push(neighbor);
                min_row = min_row.min(neighbor.0);
                max_row = max_row.max(neighbor.0);
                min_col = min_col.min(neighbor.1);
                max_col = max_col.max(neighbor.1);
                component.push(cell);
            }
        }

        component.sort_by_key(|cell| (cell.row, cell.col));
        regions.push(Region {
            range: range_address(min_row, min_col, max_row, max_col),
            cells: component,
            min_row,
            max_row,
            min_col,
            max_col,
        });
    }

    regions.sort_by_key(|region| (region.min_row, region.min_col));
    regions
}

fn sheet_bounds(cells: &[PreviewCell]) -> (String, String, u32, u32) {
    if cells.is_empty() {
        return ("A1:A1".to_string(), "A1".to_string(), 1, 1);
    }
    let min_row = cells.iter().map(|cell| cell.row).min().unwrap_or(1);
    let max_row = cells.iter().map(|cell| cell.row).max().unwrap_or(1);
    let min_col = cells.iter().map(|cell| cell.col).min().unwrap_or(1);
    let max_col = cells.iter().map(|cell| cell.col).max().unwrap_or(1);
    (
        range_address(min_row, min_col, max_row, max_col),
        cell_address(min_row, min_col),
        max_row,
        max_col,
    )
}

fn looks_like_single_table(sheet: &SheetPreview) -> bool {
    if sheet.cells.is_empty() {
        return false;
    }
    let min_row = sheet.cells.iter().map(|cell| cell.row).min().unwrap_or(1);
    let max_row = sheet.cells.iter().map(|cell| cell.row).max().unwrap_or(1);
    let min_col = sheet.cells.iter().map(|cell| cell.col).min().unwrap_or(1);
    let max_col = sheet.cells.iter().map(|cell| cell.col).max().unwrap_or(1);
    let row_count = max_row - min_row + 1;
    let column_count = max_col - min_col + 1;
    let area = row_count * column_count;
    area >= 4
        && row_count >= 2
        && column_count >= 2
        && (sheet.cells.len() as f64 / area as f64) >= 0.5
}

fn region_role(region: &Region) -> &'static str {
    let row_count = region.max_row - region.min_row + 1;
    let column_count = region.max_col - region.min_col + 1;
    if row_count == 1 && column_count > 1 {
        "title_or_header_band"
    } else if row_count >= 2 && column_count >= 2 {
        "table_like_region"
    } else {
        "cell_group"
    }
}

fn cell_json_value(cell: &PreviewCell) -> Option<JsonValue> {
    let value = cell.value.as_ref()?;
    match value {
        Data::Empty => None,
        Data::String(value) => Some(limited_string_value(value)),
        Data::Int(value) => Some(JsonValue::I64(*value)),
        Data::Float(value) => {
            if value.is_finite()
                && value.fract() == 0.0
                && *value >= i64::MIN as f64
                && *value <= i64::MAX as f64
            {
                Some(JsonValue::I64(*value as i64))
            } else {
                Some(JsonValue::F64(*value))
            }
        }
        Data::Bool(value) => Some(JsonValue::Bool(*value)),
        Data::DateTime(value) => Some(JsonValue::String(value.to_string())),
        Data::DateTimeIso(value) => Some(limited_string_value(value)),
        Data::DurationIso(value) => Some(limited_string_value(value)),
        Data::Error(value) => Some(JsonValue::String(value.to_string())),
    }
}

fn limited_string_value(value: &str) -> JsonValue {
    if value.chars().count() <= SAMPLE_TEXT_LIMIT {
        JsonValue::String(value.to_string())
    } else {
        JsonValue::String(value.chars().take(SAMPLE_TEXT_LIMIT).collect())
    }
}

fn is_long_text_cell(cell: &PreviewCell) -> bool {
    match cell.value.as_ref() {
        Some(Data::String(value)) => value.chars().count() > SAMPLE_TEXT_LIMIT,
        Some(value) => value.to_string().chars().count() > SAMPLE_TEXT_LIMIT,
        None => false,
    }
}

fn markdown_block(part: &str, content: String) -> Block {
    Block {
        label: part.to_string(),
        content,
        truncated: false,
    }
}

fn structured_block(part: &str, sheet: Option<&str>, records: Vec<String>) -> Block {
    let label = sheet
        .map(|sheet| format!("{part}:{sheet}"))
        .unwrap_or_else(|| part.to_string());
    Block {
        label,
        content: records.join("\n"),
        truncated: false,
    }
}

fn compose_llm_preview(blocks: &[Block], max_chars: usize) -> (String, bool) {
    let mut parts = Vec::new();
    let mut used = 0usize;

    for block in blocks {
        let mut content = block.content.clone();
        if block.truncated && !content.contains("[truncated]") {
            content.push_str("\n[truncated]");
        }
        let section = format!("## {}\n{}", block.label, content);
        let separator = if parts.is_empty() { "" } else { "\n\n" };
        let available = max_chars.saturating_sub(used + separator.len());
        if available == 0 {
            return (with_truncation_marker(&parts.join(""), max_chars), true);
        }
        if section.len() > available {
            let suffix = "\n[truncated]";
            if available > suffix.len() {
                parts.push(format!(
                    "{separator}{}{}",
                    &section[..available - suffix.len()],
                    suffix
                ));
                return (parts.join("").chars().take(max_chars).collect(), true);
            }
            return (with_truncation_marker(&parts.join(""), max_chars), true);
        }
        used += separator.len() + section.len();
        parts.push(format!("{separator}{section}"));
    }

    (parts.join(""), false)
}

fn with_truncation_marker(text: &str, max_chars: usize) -> String {
    let marker = "\n[truncated]";
    if text.contains("[truncated]") {
        return text.chars().take(max_chars).collect();
    }
    let keep = max_chars.saturating_sub(marker.len());
    format!("{}{}", text.chars().take(keep).collect::<String>(), marker)
}

fn normalize_formula(formula: &str) -> String {
    if formula.starts_with('=') {
        formula.to_string()
    } else {
        format!("={formula}")
    }
}

fn qualified_cell_address(sheet_name: &str, row: u32, col: u32) -> String {
    format!(
        "{}!{}",
        sheet_reference_name(sheet_name),
        cell_address(row, col)
    )
}

fn sheet_reference_name(sheet_name: &str) -> String {
    if can_use_unquoted_sheet_name(sheet_name) {
        return sheet_name.to_string();
    }
    format!("'{}'", sheet_name.replace('\'', "''"))
}

fn can_use_unquoted_sheet_name(sheet_name: &str) -> bool {
    let mut chars = sheet_name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn range_address(min_row: u32, min_col: u32, max_row: u32, max_col: u32) -> String {
    format!(
        "{}:{}",
        cell_address(min_row, min_col),
        cell_address(max_row, max_col)
    )
}

fn cell_address(row: u32, col: u32) -> String {
    format!("{}{}", column_letter(col), row)
}

fn column_letter(mut col: u32) -> String {
    let mut chars = Vec::new();
    while col > 0 {
        col -= 1;
        chars.push((b'A' + (col % 26) as u8) as char);
        col /= 26;
    }
    chars.iter().rev().collect()
}

fn json_record(fields: Vec<(&'static str, JsonValue)>) -> String {
    let mut out = String::from("{");
    for (idx, (key, value)) in fields.into_iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&serde_json::to_string(key).expect("json key serialization cannot fail"));
        out.push(':');
        out.push_str(&json_value(value));
    }
    out.push('}');
    out
}

fn json_value(value: JsonValue) -> String {
    match value {
        JsonValue::String(value) => {
            serde_json::to_string(&value).expect("json string serialization cannot fail")
        }
        JsonValue::Bool(value) => value.to_string(),
        JsonValue::U64(value) => value.to_string(),
        JsonValue::I64(value) => value.to_string(),
        JsonValue::F64(value) => {
            serde_json::to_string(&value).expect("json number serialization cannot fail")
        }
        JsonValue::Null => "null".to_string(),
    }
}
