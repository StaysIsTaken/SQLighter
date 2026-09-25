//! File format writers (export) and readers (import).

use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::db::cell_str;
use crate::model::{ColumnMeta, Dialect, ExportFormat, ExportOptions, ImportFormat, ImportSourceOptions, Row};
use crate::sql::quote_ident;

// ------------------------------------------------------------------------------------------
// Export

pub enum Writer {
    Csv(csv::Writer<BufWriter<File>>, String),
    Json { out: BufWriter<File>, cols: Vec<String>, first: bool, pretty: bool },
    Xml { out: BufWriter<File>, cols: Vec<String> },
    Xlsx { book: rust_xlsxwriter::Workbook, row: u32, path: String, sheet_rows: u32 },
    Sql { out: BufWriter<File>, target: String, cols: Vec<String>, types: Vec<Option<String>>, dialect: Dialect, batch: usize },
    Markdown { out: BufWriter<File>, null: String },
    Html { out: BufWriter<File>, null: String },
}

pub struct ExportCtx<'a> {
    pub opts: &'a ExportOptions,
    pub table_name: String,
    pub dialect: Dialect,
}

impl Writer {
    pub fn create(ctx: &ExportCtx, columns: &[ColumnMeta]) -> Result<Writer> {
        let path = &ctx.opts.file_path;
        let open = || -> Result<BufWriter<File>> { Ok(BufWriter::with_capacity(1 << 16, File::create(path).with_context(|| format!("create {path}"))?)) };
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let null = ctx.opts.null_text.clone().unwrap_or_default();
        Ok(match ctx.opts.format {
            ExportFormat::Csv | ExportFormat::Tsv => {
                let delim = match ctx.opts.format {
                    ExportFormat::Tsv => b'\t',
                    _ => ctx.opts.delimiter.as_deref().and_then(|d| d.bytes().next()).unwrap_or(b','),
                };
                let mut w = csv::WriterBuilder::new().delimiter(delim).from_writer(open()?);
                if ctx.opts.include_header {
                    w.write_record(&names)?;
                }
                Writer::Csv(w, null)
            }
            ExportFormat::Json => {
                let mut out = open()?;
                out.write_all(b"[")?;
                Writer::Json { out, cols: names, first: true, pretty: ctx.opts.pretty_json }
            }
            ExportFormat::Xml => {
                let mut out = open()?;
                writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
                writeln!(out, "<table name=\"{}\">", xml_escape(&ctx.table_name))?;
                Writer::Xml { out, cols: names }
            }
            ExportFormat::Xlsx => {
                let mut book = rust_xlsxwriter::Workbook::new();
                let sheet = book.add_worksheet();
                sheet.set_name(sheet_name(&ctx.table_name))?;
                let bold = rust_xlsxwriter::Format::new().set_bold();
                let mut row = 0;
                if ctx.opts.include_header {
                    for (i, n) in names.iter().enumerate() {
                        sheet.write_string_with_format(0, i as u16, n, &bold)?;
                    }
                    sheet.set_freeze_panes(1, 0)?;
                    row = 1;
                }
                Writer::Xlsx { book, row, path: path.clone(), sheet_rows: 0 }
            }
            ExportFormat::Sql => {
                let mut out = open()?;
                writeln!(out, "-- Exported by SQLighter on {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"))?;
                let target = if ctx.table_name.contains('.') || ctx.table_name.starts_with(['"', '`', '[']) { ctx.table_name.clone() } else { quote_ident(&ctx.table_name, ctx.dialect) };
                Writer::Sql {
                    out,
                    target,
                    cols: names,
                    types: columns.iter().map(|c| c.type_name.clone()).collect(),
                    dialect: ctx.dialect,
                    batch: ctx.opts.batch_size.unwrap_or(100).max(1),
                }
            }
            ExportFormat::Markdown => {
                let mut out = open()?;
                writeln!(out, "| {} |", names.iter().map(|n| md_escape(n)).collect::<Vec<_>>().join(" | "))?;
                writeln!(out, "|{}|", names.iter().map(|_| " --- ").collect::<Vec<_>>().join("|"))?;
                Writer::Markdown { out, null }
            }
            ExportFormat::Html => {
                let mut out = open()?;
                write!(
                    out,
                    "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\"><title>{t}</title><style>body{{font-family:system-ui,sans-serif;margin:24px}}table{{border-collapse:collapse;font-size:13px}}th,td{{border:1px solid #ddd;padding:4px 8px;text-align:left}}th{{background:#f4f4f5}}td.null{{color:#999;font-style:italic}}</style></head><body><h2>{t}</h2><table><thead><tr>",
                    t = html_escape(&ctx.table_name)
                )?;
                for n in &names {
                    write!(out, "<th>{}</th>", html_escape(n))?;
                }
                writeln!(out, "</tr></thead><tbody>")?;
                Writer::Html { out, null }
            }
        })
    }

    pub fn write_rows(&mut self, rows: &[Row]) -> Result<()> {
        match self {
            Writer::Csv(w, null) => {
                for r in rows {
                    w.write_record(r.iter().map(|v| if v.is_null() { null.clone() } else { cell_str(v) }))?;
                }
            }
            Writer::Json { out, cols, first, pretty } => {
                for r in rows {
                    let obj: serde_json::Map<String, Value> = cols.iter().cloned().zip(r.iter().cloned()).collect();
                    if !*first {
                        out.write_all(b",")?;
                    }
                    *first = false;
                    if *pretty {
                        out.write_all(b"\n  ")?;
                        out.write_all(serde_json::to_string_pretty(&obj)?.replace('\n', "\n  ").as_bytes())?;
                    } else {
                        out.write_all(b"\n")?;
                        serde_json::to_writer(&mut *out, &obj)?;
                    }
                }
            }
            Writer::Xml { out, cols } => {
                for r in rows {
                    out.write_all(b"  <row>\n")?;
                    for (i, v) in r.iter().enumerate() {
                        let tag = xml_tag(cols.get(i).map(|s| s.as_str()).unwrap_or("col"));
                        if v.is_null() {
                            writeln!(out, "    <{tag} null=\"true\"/>")?;
                        } else {
                            writeln!(out, "    <{tag}>{}</{tag}>", xml_escape(&cell_str(v)))?;
                        }
                    }
                    out.write_all(b"  </row>\n")?;
                }
            }
            Writer::Xlsx { book, row, sheet_rows, .. } => {
                let sheet = book.worksheet_from_index(0)?;
                for r in rows {
                    if *row >= 1_048_575 {
                        bail!("Excel supports at most 1,048,576 rows per sheet; use CSV for larger exports");
                    }
                    for (i, v) in r.iter().enumerate() {
                        let c = i as u16;
                        match v {
                            Value::Null => {}
                            Value::Bool(b) => {
                                sheet.write_boolean(*row, c, *b)?;
                            }
                            Value::Number(n) => {
                                sheet.write_number(*row, c, n.as_f64().unwrap_or(0.0))?;
                            }
                            other => {
                                let s = cell_str(other);
                                let s = if s.chars().count() > 32_000 { s.chars().take(32_000).collect() } else { s };
                                sheet.write_string(*row, c, s)?;
                            }
                        }
                    }
                    *row += 1;
                    *sheet_rows += 1;
                }
            }
            Writer::Sql { out, target, cols, types, dialect, batch } => {
                for s in crate::sqlgen::inserts(target, cols, types, rows, *dialect, *batch) {
                    out.write_all(s.as_bytes())?;
                    out.write_all(b";\n")?;
                }
            }
            Writer::Markdown { out, null } => {
                for r in rows {
                    let cells: Vec<String> = r.iter().map(|v| if v.is_null() { null.clone() } else { md_escape(&cell_str(v)) }).collect();
                    writeln!(out, "| {} |", cells.join(" | "))?;
                }
            }
            Writer::Html { out, null } => {
                for r in rows {
                    out.write_all(b"<tr>")?;
                    for v in r {
                        if v.is_null() {
                            write!(out, "<td class=\"null\">{}</td>", html_escape(null))?;
                        } else {
                            write!(out, "<td>{}</td>", html_escape(&cell_str(v)))?;
                        }
                    }
                    out.write_all(b"</tr>\n")?;
                }
            }
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        match self {
            Writer::Csv(mut w, _) => w.flush()?,
            Writer::Json { mut out, .. } => {
                out.write_all(b"\n]\n")?;
                out.flush()?
            }
            Writer::Xml { mut out, .. } => {
                out.write_all(b"</table>\n")?;
                out.flush()?
            }
            Writer::Xlsx { mut book, path, .. } => {
                if let Ok(sheet) = book.worksheet_from_index(0) {
                    let _ = sheet.autofit();
                }
                book.save(&path)?
            }
            Writer::Sql { mut out, .. } => out.flush()?,
            Writer::Markdown { mut out, .. } => out.flush()?,
            Writer::Html { mut out, .. } => {
                out.write_all(b"</tbody></table></body></html>\n")?;
                out.flush()?
            }
        }
        Ok(())
    }
}

fn sheet_name(s: &str) -> String {
    let n: String = s.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(31).collect();
    if n.is_empty() {
        "Export".into()
    } else {
        n
    }
}

pub fn xml_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            // Characters not allowed in XML 1.0 are dropped.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c => o.push(c),
        }
    }
    o
}

fn html_escape(s: &str) -> String {
    xml_escape(s)
}

fn md_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('|', "\\|").replace('\n', "<br>").replace('\r', "")
}

/// Valid XML element name for a column.
fn xml_tag(name: &str) -> String {
    let mut t: String = name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect();
    if t.is_empty() || !(t.chars().next().unwrap().is_alphabetic() || t.starts_with('_')) || t.to_ascii_lowercase().starts_with("xml") {
        t = format!("_{t}");
    }
    t
}

// ------------------------------------------------------------------------------------------
// Import

pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub sheets: Option<Vec<String>>,
}

pub fn read_table(o: &ImportSourceOptions, limit: Option<usize>) -> Result<Table> {
    let path = Path::new(&o.file_path);
    match o.format {
        ImportFormat::Csv | ImportFormat::Tsv => read_csv(path, o, limit),
        ImportFormat::Json => read_json(path, limit),
        ImportFormat::Xml => read_xml(path, o.xml_row_tag.as_deref(), limit),
        ImportFormat::Xlsx => read_xlsx(path, o, limit),
    }
}

fn read_csv(path: &Path, o: &ImportSourceOptions, limit: Option<usize>) -> Result<Table> {
    let delim = match o.format {
        ImportFormat::Tsv => b'\t',
        _ => match o.delimiter.as_deref() {
            Some("\\t") => b'\t',
            Some(d) if !d.is_empty() => d.as_bytes()[0],
            _ => sniff_delimiter(path).unwrap_or(b','),
        },
    };
    let mut r = csv::ReaderBuilder::new().delimiter(delim).has_headers(false).flexible(true).from_reader(BufReader::new(File::open(path)?));
    let mut columns = Vec::new();
    let mut rows = Vec::new();
    for (i, rec) in r.records().enumerate() {
        let rec = rec?;
        if i == 0 && o.has_header {
            columns = rec.iter().map(|s| s.trim_start_matches('\u{feff}').to_string()).collect();
            continue;
        }
        if limit.is_some_and(|l| rows.len() >= l) {
            break;
        }
        rows.push(rec.iter().map(|s| if s.is_empty() { Value::Null } else { Value::String(s.to_string()) }).collect::<Row>());
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(columns.len());
    while columns.len() < width {
        columns.push(format!("column{}", columns.len() + 1));
    }
    for r in rows.iter_mut() {
        r.resize(width, Value::Null);
    }
    Ok(Table { columns, rows, sheets: None })
}

fn sniff_delimiter(path: &Path) -> Option<u8> {
    use std::io::BufRead;
    let f = File::open(path).ok()?;
    let mut line = String::new();
    BufReader::new(f).read_line(&mut line).ok()?;
    [b',', b';', b'\t', b'|'].into_iter().max_by_key(|d| line.bytes().filter(|b| b == d).count())
}

fn read_json(path: &Path, limit: Option<usize>) -> Result<Table> {
    let v: Value = serde_json::from_reader(BufReader::new(File::open(path)?)).context("invalid JSON")?;
    let items = match v {
        Value::Array(a) => a,
        Value::Object(mut m) => {
            // {"rows": [...]} or {"data": [...]} or first array property
            let key = m.iter().find(|(_, v)| v.is_array()).map(|(k, _)| k.clone());
            match key.and_then(|k| m.remove(&k)) {
                Some(Value::Array(a)) => a,
                _ => vec![Value::Object(m)],
            }
        }
        _ => bail!("JSON must be an array of objects"),
    };
    let mut columns: Vec<String> = Vec::new();
    for it in items.iter().take(1000) {
        if let Value::Object(m) = it {
            for k in m.keys() {
                if !columns.contains(k) {
                    columns.push(k.clone());
                }
            }
        }
    }
    if columns.is_empty() && items.iter().any(|x| x.is_array()) {
        let w = items.iter().filter_map(|x| x.as_array().map(|a| a.len())).max().unwrap_or(0);
        columns = (1..=w).map(|i| format!("column{i}")).collect();
    }
    let take = limit.unwrap_or(usize::MAX);
    let rows = items
        .into_iter()
        .take(take)
        .map(|it| match it {
            Value::Object(m) => columns.iter().map(|c| flatten(m.get(c).cloned().unwrap_or(Value::Null))).collect(),
            Value::Array(a) => {
                let mut r: Row = a.into_iter().map(flatten).collect();
                r.resize(columns.len(), Value::Null);
                r
            }
            other => vec![flatten(other)],
        })
        .collect();
    Ok(Table { columns, rows, sheets: None })
}

fn flatten(v: Value) -> Value {
    match v {
        Value::Array(_) | Value::Object(_) => Value::String(v.to_string()),
        other => other,
    }
}

/// Reads XML shaped like `<root><row><col>v</col>...</row>...</root>` (also attributes on rows).
fn read_xml(path: &Path, row_tag: Option<&str>, limit: Option<usize>) -> Result<Table> {
    use quick_xml::events::Event;
    let data = std::fs::read_to_string(path)?;
    // Auto-detect row element: the most frequent element at depth 2.
    let row_tag = match row_tag.filter(|s| !s.is_empty()) {
        Some(t) => t.to_string(),
        None => {
            let mut counts: std::collections::HashMap<String, usize> = Default::default();
            let mut rd = quick_xml::Reader::from_str(&data);
            let mut depth = 0;
            loop {
                match rd.read_event()? {
                    Event::Start(e) => {
                        depth += 1;
                        if depth == 2 {
                            *counts.entry(e.name().as_ref().to_string()).or_default() += 1;
                        }
                    }
                    Event::Empty(e) => {
                        if depth == 1 {
                            *counts.entry(e.name().as_ref().to_string()).or_default() += 1;
                        }
                    }
                    Event::End(_) => depth -= 1,
                    Event::Eof => break,
                    _ => {}
                }
            }
            counts.into_iter().max_by_key(|(_, c)| *c).map(|(k, _)| k).context("no rows found in XML")?
        }
    };
    let mut rd = quick_xml::Reader::from_str(&data);
    let mut columns: Vec<String> = Vec::new();
    let mut records: Vec<Vec<(String, Value)>> = Vec::new();
    let mut cur: Option<Vec<(String, Value)>> = None;
    let mut field: Option<(String, String, bool)> = None;
    let max = limit.unwrap_or(usize::MAX);
    let add_col = |columns: &mut Vec<String>, k: &str| {
        if !columns.iter().any(|c| c == k) {
            columns.push(k.to_string());
        }
    };
    loop {
        let ev = rd.read_event()?;
        match ev {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let name = e.name().as_ref().to_string();
                let empty = matches!(ev, Event::Empty(_));
                if cur.is_none() && name == row_tag {
                    let mut rec = Vec::new();
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref().to_string();
                        let v = a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|x| x.into_owned()).unwrap_or_default();
                        add_col(&mut columns, &k);
                        rec.push((k, Value::String(v)));
                    }
                    if empty {
                        records.push(rec);
                    } else {
                        cur = Some(rec);
                    }
                } else if cur.is_some() && field.is_none() {
                    let is_null = e.attributes().flatten().any(|a| a.key.as_ref() == "null" && a.value.as_ref() == "true");
                    add_col(&mut columns, &name);
                    if empty {
                        cur.as_mut().unwrap().push((name, if is_null { Value::Null } else { Value::String(String::new()) }));
                    } else {
                        field = Some((name, String::new(), is_null));
                    }
                }
            }
            Event::Text(t) => {
                if let Some((_, buf, _)) = field.as_mut() {
                    buf.push_str(&t.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some((_, buf, _)) = field.as_mut() {
                    let name = r.to_string();
                    let rep = match name.as_str() {
                        "lt" => "<".to_string(),
                        "gt" => ">".to_string(),
                        "amp" => "&".to_string(),
                        "quot" => "\"".to_string(),
                        "apos" => "'".to_string(),
                        n if n.starts_with("#x") => u32::from_str_radix(&n[2..], 16).ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
                        n if n.starts_with('#') => n[1..].parse::<u32>().ok().and_then(char::from_u32).map(String::from).unwrap_or_default(),
                        _ => String::new(),
                    };
                    buf.push_str(&rep);
                }
            }
            Event::CData(t) => {
                if let Some((_, buf, _)) = field.as_mut() {
                    buf.push_str(&t);
                }
            }
            Event::End(e) => {
                let name = e.name().as_ref().to_string();
                if let Some((fname, buf, is_null)) = field.take() {
                    if fname == name {
                        cur.as_mut().unwrap().push((fname, if is_null { Value::Null } else { Value::String(buf) }));
                    } else {
                        field = Some((fname, buf, is_null));
                    }
                } else if name == row_tag {
                    if let Some(rec) = cur.take() {
                        records.push(rec);
                        if records.len() >= max {
                            break;
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let rows = records
        .into_iter()
        .map(|rec| {
            let mut r = vec![Value::Null; columns.len()];
            for (k, v) in rec {
                if let Some(i) = columns.iter().position(|c| *c == k) {
                    r[i] = v;
                }
            }
            r
        })
        .collect();
    Ok(Table { columns, rows, sheets: None })
}

fn read_xlsx(path: &Path, o: &ImportSourceOptions, limit: Option<usize>) -> Result<Table> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut wb = open_workbook_auto(path).context("open spreadsheet")?;
    let sheets = wb.sheet_names().to_vec();
    let sheet = o.sheet.clone().filter(|s| sheets.contains(s)).or_else(|| sheets.first().cloned()).context("workbook has no sheets")?;
    let range = wb.worksheet_range(&sheet)?;
    let mut it = range.rows();
    let mut columns: Vec<String> = Vec::new();
    if o.has_header {
        if let Some(h) = it.next() {
            columns = h.iter().enumerate().map(|(i, c)| { let s = c.to_string(); if s.is_empty() { format!("column{}", i + 1) } else { s } }).collect();
        }
    }
    let max = limit.unwrap_or(usize::MAX);
    let mut rows = Vec::new();
    for r in it {
        if rows.len() >= max {
            break;
        }
        let row: Row = r
            .iter()
            .map(|c| match c {
                Data::Empty => Value::Null,
                Data::Int(i) => Value::from(*i),
                Data::Float(f) => {
                    if f.fract() == 0.0 && f.abs() < 9e15 {
                        Value::from(*f as i64)
                    } else {
                        serde_json::Number::from_f64(*f).map(Value::Number).unwrap_or(Value::Null)
                    }
                }
                Data::Bool(b) => Value::Bool(*b),
                Data::DateTime(d) => d
                    .as_datetime()
                    .map(|dt| {
                        if dt.time() == chrono::NaiveTime::MIN {
                            Value::String(dt.date().to_string())
                        } else {
                            Value::String(dt.format("%Y-%m-%d %H:%M:%S").to_string())
                        }
                    })
                    .unwrap_or(Value::Null),
                Data::DateTimeIso(s) | Data::DurationIso(s) | Data::String(s) => Value::String(s.clone()),
                Data::Error(_) => Value::Null,
            })
            .collect();
        if row.iter().all(|v| v.is_null()) {
            continue;
        }
        rows.push(row);
    }
    let width = rows.iter().map(|r| r.len()).max().unwrap_or(0).max(columns.len());
    while columns.len() < width {
        columns.push(format!("column{}", columns.len() + 1));
    }
    for r in rows.iter_mut() {
        r.resize(width, Value::Null);
    }
    Ok(Table { columns, rows, sheets: Some(sheets) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> String {
        std::env::temp_dir().join(format!("sqlighter-test-{}-{name}", std::process::id())).to_string_lossy().into_owned()
    }

    fn roundtrip(format: ExportFormat, imp: ImportFormat, ext: &str) {
        let path = tmp(&format!("rt.{ext}"));
        let opts = ExportOptions { format, file_path: path.clone(), delimiter: None, include_header: true, null_text: None, batch_size: None, pretty_json: true };
        let ctx = ExportCtx { opts: &opts, table_name: "people".into(), dialect: Dialect::Postgres };
        let cols = vec![ColumnMeta { name: "id".into(), type_name: None }, ColumnMeta { name: "name".into(), type_name: None }];
        let mut w = Writer::create(&ctx, &cols).unwrap();
        w.write_rows(&[vec![Value::from(1), Value::from("Ä <b>&\"x\"")], vec![Value::from(2), Value::Null]]).unwrap();
        w.finish().unwrap();
        let t = read_table(&ImportSourceOptions { format: imp, file_path: path.clone(), delimiter: None, has_header: true, sheet: None, xml_row_tag: None }, None).unwrap();
        assert_eq!(t.columns, vec!["id", "name"], "{format:?}");
        assert_eq!(t.rows.len(), 2);
        assert_eq!(cell_str(&t.rows[0][1]), "Ä <b>&\"x\"", "{format:?}");
        assert!(t.rows[1][1].is_null(), "{format:?}");
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn csv_roundtrip() {
        roundtrip(ExportFormat::Csv, ImportFormat::Csv, "csv");
    }
    #[test]
    fn tsv_roundtrip() {
        roundtrip(ExportFormat::Tsv, ImportFormat::Tsv, "tsv");
    }
    #[test]
    fn json_roundtrip() {
        roundtrip(ExportFormat::Json, ImportFormat::Json, "json");
    }
    #[test]
    fn xml_roundtrip() {
        roundtrip(ExportFormat::Xml, ImportFormat::Xml, "xml");
    }
    #[test]
    fn xlsx_roundtrip() {
        roundtrip(ExportFormat::Xlsx, ImportFormat::Xlsx, "xlsx");
    }

    #[test]
    fn xml_tags() {
        assert_eq!(xml_tag("first name"), "first_name");
        assert_eq!(xml_tag("1st"), "_1st");
        assert_eq!(xml_tag("xmlthing"), "_xmlthing");
    }
}
