//! Reading documents the agent cannot parse itself.
//!
//! An agent's whole view of a project is `read_file`, which returns UTF-8. That
//! is fine for source but useless for the formats people actually carry around
//! — a PDF read as UTF-8 is either an error or a screenful of mojibake. So the
//! binary ones are converted to text at the point of reading, and the agent
//! never learns the difference.
//!
//! Extraction happens on read rather than on import, deliberately. Importing a
//! PDF keeps the PDF: it stays exportable, it stays pushable to GitHub, and the
//! file the user recognises is the file they see in the Files tab. Converting
//! at import would leave them with a `.txt` they never asked for and no way
//! back.

use std::io::{Cursor, Read};

use crate::error::{Result, VonError};

/// Extracted text is capped well below the observation limit, so a 300-page PDF
/// cannot evict a whole conversation from the context window in one call.
const MAX_EXTRACTED_CHARS: usize = 200_000;

/// Refuse rather than spend a phone's memory unpacking something absurd.
const MAX_DOCUMENT_BYTES: usize = 32 * 1024 * 1024;

/// Formats worth converting. Everything else is either already text or is
/// something we should say we cannot read, rather than guess at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Document {
    /// Already UTF-8; `read_file` handles it without help.
    PlainText,
    Pdf,
    Docx,
    /// A format we can name but not read, so the error can say what to do.
    Unsupported(&'static str),
}

/// What a file's extension says it is.
pub fn classify(path: &str) -> Document {
    let ext = path
        .rsplit('.')
        .next()
        .filter(|e| !e.eq_ignore_ascii_case(path))
        .unwrap_or_default()
        .to_ascii_lowercase();

    match ext.as_str() {
        "pdf" => Document::Pdf,
        "docx" => Document::Docx,
        // The pre-2007 binary formats are a different problem entirely — OLE
        // compound files, not zip — and are not worth carrying a parser for.
        "doc" => Document::Unsupported("Word 97-2003 (.doc)"),
        "xls" => Document::Unsupported("Excel 97-2003 (.xls)"),
        "ppt" => Document::Unsupported("PowerPoint 97-2003 (.ppt)"),
        "rtf" => Document::Unsupported("Rich Text (.rtf)"),
        "epub" => Document::Unsupported("EPUB"),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => {
            Document::Unsupported("an image")
        }
        "zip" | "gz" | "tar" | "7z" | "rar" => Document::Unsupported("an archive"),
        "mp3" | "mp4" | "wav" | "mov" | "m4a" => Document::Unsupported("a media file"),
        _ => Document::PlainText,
    }
}

/// Whether reading this path needs conversion rather than a plain UTF-8 read.
pub fn needs_extraction(path: &str) -> bool {
    matches!(classify(path), Document::Pdf | Document::Docx)
}

/// Converts a document's bytes to text.
pub fn extract(path: &str, bytes: &[u8]) -> Result<String> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(VonError::QuotaExceeded(format!(
            "'{path}' is {} MB; VON reads documents up to {} MB",
            bytes.len() / (1024 * 1024),
            MAX_DOCUMENT_BYTES / (1024 * 1024)
        )));
    }

    let text = match classify(path) {
        Document::Pdf => extract_pdf(path, bytes)?,
        Document::Docx => extract_docx(path, bytes)?,
        Document::PlainText => String::from_utf8_lossy(bytes).into_owned(),
        Document::Unsupported(what) => {
            return Err(VonError::InvalidArgument(format!(
                "'{path}' is {what}, which VON cannot read as text. Convert it to PDF, DOCX or \
                 Markdown and import it again."
            )))
        }
    };

    Ok(tidy(&text))
}

#[cfg(feature = "pdf")]
fn extract_pdf(path: &str, bytes: &[u8]) -> Result<String> {
    // pdf-extract panics on some malformed files rather than returning an
    // error, and a panic here would take down the agent's whole run over one
    // bad attachment. It is pure computation over a byte slice with no shared
    // state, so catching is safe.
    let parsed = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes));

    match parsed {
        Ok(Ok(text)) if !text.trim().is_empty() => Ok(text),
        Ok(Ok(_)) => Err(VonError::InvalidArgument(format!(
            "'{path}' has no extractable text — it is most likely a scan. VON does not do OCR."
        ))),
        Ok(Err(e)) => Err(VonError::InvalidArgument(format!(
            "'{path}' could not be read as a PDF: {e}"
        ))),
        Err(_) => Err(VonError::InvalidArgument(format!(
            "'{path}' is malformed enough to crash the PDF parser. Re-export it and try again."
        ))),
    }
}

#[cfg(not(feature = "pdf"))]
fn extract_pdf(path: &str, _bytes: &[u8]) -> Result<String> {
    Err(VonError::InvalidArgument(format!(
        "'{path}' is a PDF, but this build was compiled without PDF support. Convert it to \
         Markdown or DOCX and import it again."
    )))
}

/// Pulls the text out of a `.docx`.
///
/// A docx is a zip holding `word/document.xml`, and the text lives in `<w:t>`
/// elements. This scans for those rather than parsing the XML properly, which
/// is the right trade here: the full schema is enormous, we want none of the
/// styling, and a scanner cannot fail on parts of the document it does not
/// understand.
fn extract_docx(path: &str, bytes: &[u8]) -> Result<String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| {
        VonError::InvalidArgument(format!("'{path}' is not a readable .docx: {e}"))
    })?;

    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| {
            VonError::InvalidArgument(format!(
                "'{path}' has no word/document.xml, so it is not a Word document"
            ))
        })?
        .read_to_string(&mut xml)
        .map_err(|e| VonError::InvalidArgument(format!("'{path}' holds invalid XML: {e}")))?;

    Ok(scrape_word_xml(&xml))
}

/// Walks the document body emitting text runs, tabs, and paragraph breaks.
fn scrape_word_xml(xml: &str) -> String {
    let mut out = String::new();
    let bytes = xml.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let Some(close) = xml[i..].find('>').map(|o| i + o) else {
            break;
        };
        let tag = &xml[i + 1..close];

        // `<w:t>` and `<w:t xml:space="preserve">` both carry text; `<w:t/>`
        // is empty and must not swallow what follows it.
        if (tag == "w:t" || tag.starts_with("w:t ")) && !tag.ends_with('/') {
            let start = close + 1;
            match xml[start..].find("</w:t>") {
                Some(offset) => {
                    out.push_str(&unescape(&xml[start..start + offset]));
                    i = start + offset + "</w:t>".len();
                    continue;
                }
                None => break,
            }
        } else if tag == "w:tab" || tag == "w:tab/" {
            out.push('\t');
        } else if tag == "w:br"
            || tag == "w:br/"
            || tag.starts_with("w:br ")
            || tag == "/w:p"
        {
            // An explicit line break and the end of a paragraph amount to the
            // same thing once the styling is gone, so they share an arm —
            // which also keeps clippy from calling the split blocks identical.
            out.push('\n');
        }

        i = close + 1;
    }
    out
}

/// The five predefined XML entities. A docx uses no others — anything exotic
/// arrives as a numeric reference, handled here too.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];

        let Some(end) = rest.find(';').filter(|e| *e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            numeric if numeric.starts_with("#x") || numeric.starts_with("#X") => {
                u32::from_str_radix(&numeric[2..], 16).ok().and_then(char::from_u32)
            }
            numeric if numeric.starts_with('#') => {
                numeric[1..].parse().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapses the runs of blank lines that both extractors leave behind, and
/// caps the result.
///
/// Page-break noise is not free: every blank line is a token, and a document
/// read into a 4k context is competing with the conversation for room.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0;

    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }

    let trimmed = out.trim();
    if trimmed.chars().count() <= MAX_EXTRACTED_CHARS {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(MAX_EXTRACTED_CHARS).collect();
    format!(
        "{cut}\n\n… [document truncated at {MAX_EXTRACTED_CHARS} characters. Ask for a specific \
         section if you need more.]"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_map_to_handlers() {
        assert_eq!(classify("report.pdf"), Document::Pdf);
        assert_eq!(classify("Notes.DOCX"), Document::Docx);
        assert_eq!(classify("readme.md"), Document::PlainText);
        assert_eq!(classify("main.rs"), Document::PlainText);
        assert!(matches!(classify("scan.png"), Document::Unsupported(_)));
        // No extension at all is a plain read, not an error — a Makefile or a
        // LICENSE is exactly the kind of file an agent needs.
        assert_eq!(classify("Makefile"), Document::PlainText);
        assert_eq!(classify("LICENSE"), Document::PlainText);
    }

    #[test]
    fn only_binary_formats_route_through_extraction() {
        assert!(needs_extraction("a.pdf"));
        assert!(needs_extraction("a.docx"));
        assert!(!needs_extraction("a.md"));
        assert!(!needs_extraction("a.png"));
    }

    #[test]
    fn word_runs_become_paragraphs() {
        let xml = r#"<w:document><w:body>
            <w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p>
            <w:p><w:r><w:t>Second</w:t></w:r></w:p>
        </w:body></w:document>"#;
        // Runs inside one paragraph join without a break; paragraphs separate.
        assert_eq!(scrape_word_xml(xml).trim(), "Hello world\nSecond");
    }

    #[test]
    fn tabs_and_breaks_survive() {
        let xml = "<w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/><w:t>c</w:t></w:r></w:p>";
        assert_eq!(scrape_word_xml(xml).trim(), "a\tb\nc");
    }

    #[test]
    fn an_empty_run_does_not_swallow_the_text_after_it() {
        // `<w:t/>` is self-closing and has no `</w:t>`; treating it as an
        // opening tag would consume the rest of the document looking for one.
        let xml = "<w:p><w:r><w:t/><w:t>kept</w:t></w:r></w:p>";
        assert_eq!(scrape_word_xml(xml).trim(), "kept");
    }

    #[test]
    fn xml_entities_are_decoded() {
        let xml = "<w:p><w:r><w:t>a &amp; b &lt;c&gt; &quot;d&quot; &#65; &#x42;</w:t></w:r></w:p>";
        assert_eq!(scrape_word_xml(xml).trim(), r#"a & b <c> "d" A B"#);
    }

    #[test]
    fn a_bare_ampersand_is_left_alone() {
        // Technically invalid XML, but Word has shipped it, and mangling the
        // surrounding text over it would be worse than passing it through.
        assert_eq!(unescape("Tom & Jerry"), "Tom & Jerry");
        assert_eq!(unescape("100% & rising"), "100% & rising");
    }

    #[test]
    fn blank_line_runs_collapse() {
        assert_eq!(tidy("a\n\n\n\n\nb"), "a\n\nb");
        assert_eq!(tidy("  a  \n  b  "), "a\n  b");
    }

    #[test]
    fn an_oversized_document_is_refused_before_it_is_parsed() {
        let huge = vec![0u8; MAX_DOCUMENT_BYTES + 1];
        let err = extract("big.pdf", &huge).unwrap_err();
        assert!(matches!(err, VonError::QuotaExceeded(_)), "got {err:?}");
    }

    #[test]
    fn an_unreadable_format_says_what_to_do_about_it() {
        let err = extract("old.doc", b"whatever").unwrap_err();
        let message = err.to_string();
        assert!(message.contains("Word 97-2003"), "{message}");
        assert!(message.contains("Convert it"), "{message}");
    }

    #[test]
    fn plain_text_passes_through_extract_unchanged() {
        assert_eq!(extract("notes.md", b"# Title\n\nBody").unwrap(), "# Title\n\nBody");
    }
}
