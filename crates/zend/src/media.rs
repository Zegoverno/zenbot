//! Bounded image and selected PDF-page inspection from the read tool's authorized byte snapshot.
//! Conversion uses contained offline utilities in a private temporary directory with CPU, memory,
//! disk, output and time bounds; original paths are never reopened by utilities.

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

pub const MAX_FILE_BYTES: usize = 20 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const MAX_PAGES: usize = 4;
const MAX_TEXT_BYTES: u64 = 40_000;

pub fn encode(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn recognized(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-") || image_type(bytes).is_some()
}

fn image_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}

fn dimensions(bytes: &[u8], mime: &str) -> Option<(u32, u32)> {
    let le16 = |offset| Some(u16::from_le_bytes(bytes.get(offset..offset+2)?.try_into().ok()?) as u32);
    let le24 = |offset| { let value = bytes.get(offset..offset+3)?; Some(u32::from_le_bytes([value[0],value[1],value[2],0])) };
    match mime {
        "image/png" if bytes.get(12..16)? == b"IHDR" => Some((u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?), u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?))),
        "image/gif" => Some((le16(6)?, le16(8)?)),
        "image/webp" => match bytes.get(12..16)? {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8L" if *bytes.get(20)? == 0x2f => {
                let value = u32::from_le_bytes(bytes.get(21..25)?.try_into().ok()?);
                Some(((value & 0x3fff) + 1, ((value >> 14) & 0x3fff) + 1))
            }
            b"VP8 " if bytes.get(23..26)? == b"\x9d\x01\x2a" => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            _ => None,
        },
        "image/jpeg" => {
            let mut offset = 2;
            while offset < bytes.len() {
                if *bytes.get(offset)? != 0xff { return None; }
                while *bytes.get(offset)? == 0xff { offset += 1; }
                let marker = *bytes.get(offset)?;
                offset += 1;
                if marker == 0xd9 || marker == 0xda { return None; }
                if marker == 0x01 || (0xd0..=0xd8).contains(&marker) { continue; }
                let length = u16::from_be_bytes(bytes.get(offset..offset+2)?.try_into().ok()?) as usize;
                if length < 2 || offset.checked_add(length)? > bytes.len() { return None; }
                if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                    if length < 8 { return None; }
                    return Some((u16::from_be_bytes(bytes.get(offset+5..offset+7)?.try_into().ok()?) as u32, u16::from_be_bytes(bytes.get(offset+3..offset+5)?.try_into().ok()?) as u32));
                }
                offset += length;
            }
            None
        },
        _ => None,
    }
}

fn valid_dimensions(bytes: &[u8], mime: &str) -> bool {
    dimensions(bytes, mime).is_some_and(|(width,height)| width > 0 && height > 0 && width <= 10000 && height <= 10000 && u64::from(width)*u64::from(height) <= 25_000_000)
}

/// Reject unsupported, oversized or malformed external MCP image payloads before forwarding.
pub fn validate_image_block(block: &Value) -> bool {
    let Some(data) = block["data"]
        .as_str()
        .filter(|data| data.len() <= MAX_IMAGE_BYTES.div_ceil(3) * 4)
    else {
        return false;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return false;
    };
    block["type"] == "image"
        && bytes.len() <= MAX_IMAGE_BYTES
        && image_type(&bytes).is_some_and(|mime| block["mimeType"] == mime && valid_dimensions(&bytes,mime))
}

fn image(bytes: &[u8], mime: &str) -> Result<Value> {
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!("Image exceeds 5 MiB; resize it before reading.");
    }
    if !valid_dimensions(bytes, mime) { bail!("Invalid or oversized image dimensions (maximum 10000 per side and 25 megapixels)"); }
    Ok(json!({ "type": "image", "mimeType": mime, "data": encode(bytes) }))
}

fn pages(args: &Value) -> Result<Vec<u32>> {
    let Some(value) = args.get("pages") else {
        return Ok(vec![1]);
    };
    let Some(values) = value.as_array() else {
        bail!("pages must be an array of 1-based PDF page numbers");
    };
    if values.is_empty() || values.len() > MAX_PAGES {
        bail!("Select between 1 and {MAX_PAGES} PDF pages per read");
    }
    let mut result = Vec::new();
    for value in values {
        let number = value
            .as_u64()
            .filter(|n| (1..=100_000).contains(n))
            .context("PDF page numbers must be integers between 1 and 100000")?
            as u32;
        if !result.contains(&number) {
            result.push(number);
        }
    }
    Ok(result)
}

struct TempDir(PathBuf);
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn command(dir: &Path, program: &str, args: &[&std::ffi::OsStr], limit: u64) -> Result<Vec<u8>> {
    if !Path::new("/usr/bin").join(program).is_file() && !Path::new("/bin").join(program).is_file() {
        bail!("PDF reading requires poppler-utils (pdftotext and pdftoppm)");
    }
    let filter = crate::tools::contained_filter(false).map_err(anyhow::Error::msg)?;
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home".into()));
    let mut process = crate::tools::contained_command(&filter, dir, true, None, None, &[home]);
    process.env_clear().env("PATH", "/usr/bin:/bin").env("HOME", "/tmp").arg(program).args(args);
    // Parser bugs must not turn a bounded document read into unbounded CPU, RAM or disk use.
    unsafe { process.pre_exec(|| {
        for (resource, limit) in [(libc::RLIMIT_CPU, 20), (libc::RLIMIT_AS, 512 * 1024 * 1024), (libc::RLIMIT_FSIZE, 10 * 1024 * 1024)] {
            let bound = libc::rlimit { rlim_cur: limit, rlim_max: limit };
            if libc::setrlimit(resource, &bound) != 0 { return Err(std::io::Error::last_os_error()); }
        }
        Ok(())
    }); }
    let mut child = process
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "Starting {program}; PDF reading requires poppler-utils (pdftotext and pdftoppm)"
            )
        })?;
    let mut stdout = child
        .stdout
        .take()
        .context("converter stdout")?
        .take(limit + 1);
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), async {
        stdout.read_to_end(&mut out).await?;
        if out.len() as u64 > limit {
            bail!("{program} output exceeded the read limit");
        }
        if !child.wait().await?.success() {
            bail!("{program} failed; check the PDF and selected page numbers");
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context("PDF conversion timed out after 30 seconds")??;
    Ok(out)
}

/// None means ordinary text/unsupported binary. Call only after the read tool checks path access,
/// exact-snapshot provenance and source restrictions; returned media retains those restrictions.
pub async fn read(bytes: &[u8], args: &Value) -> Result<Option<(String, Vec<Value>)>> {
    if let Some(mime) = image_type(bytes) {
        return Ok(Some((
            format!("Image ({mime}, {} bytes).", bytes.len()),
            vec![image(bytes, mime)?],
        )));
    }
    if !bytes.starts_with(b"%PDF-") {
        return Ok(None);
    }
    if bytes.len() > MAX_FILE_BYTES {
        bail!("PDF exceeds 20 MiB");
    }
    let pages = pages(args)?;
    let dir = std::env::temp_dir().join(format!("zen-media-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let cleanup = TempDir(dir);
    let input = cleanup.0.join("input.pdf");
    {
        use std::io::Write;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&input)?
            .write_all(bytes)?;
    }
    let mut text = String::new();
    let mut blocks = Vec::new();
    let mut total = 0;
    for page in pages {
        let number = page.to_string();
        let extracted = command(
            &cleanup.0,
            "pdftotext",
            &[
                "-f".as_ref(),
                number.as_ref(),
                "-l".as_ref(),
                number.as_ref(),
                "-layout".as_ref(),
                input.as_os_str(),
                "-".as_ref(),
            ],
            MAX_TEXT_BYTES,
        )
        .await?;
        text.push_str(&format!(
            "\nPDF page {page}:\n{}\n",
            String::from_utf8_lossy(&extracted)
        ));
        let prefix = cleanup.0.join(format!("page-{page}"));
        command(
            &cleanup.0,
            "pdftoppm",
            &[
                "-f".as_ref(),
                number.as_ref(),
                "-l".as_ref(),
                number.as_ref(),
                "-singlefile".as_ref(),
                "-scale-to".as_ref(),
                "1600".as_ref(),
                "-png".as_ref(),
                input.as_os_str(),
                prefix.as_os_str(),
            ],
            1024,
        )
        .await?;
        let rendered = bounded_file(&prefix.with_extension("png")).await?;
        total += rendered.len();
        if total > 10 * 1024 * 1024 {
            bail!("Rendered pages exceed 10 MiB; select fewer pages");
        }
        blocks.push(json!({ "type": "text", "text": format!("PDF page {page}") }));
        blocks.push(image(&rendered, "image/png")?);
    }
    Ok(Some((text, blocks)))
}

async fn bounded_file(path: &Path) -> Result<Vec<u8>> {
    let file = tokio::fs::File::open(path).await?;
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > MAX_IMAGE_BYTES {
        bail!("Rendered PDF page exceeds 5 MiB");
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        bytes.extend(width.to_be_bytes()); bytes.extend(height.to_be_bytes()); bytes
    }

    #[test]
    fn signatures_and_selection_are_bounded() {
        assert_eq!(image_type(b"RIFF0000WEBP"), Some("image/webp"));
        assert_eq!(image_type(b"ordinary text"), None);
        assert!(image(&png_header(10000,10000), "image/png").is_err());
        assert!(image(&png_header(0,1), "image/png").is_err());
        assert!(image(b"\x89PNG\r\n\x1a\n", "image/png").is_err());
        assert_eq!(dimensions(&png_header(64,32), "image/png"), Some((64,32)));
        assert_eq!(dimensions(b"GIF89a\x40\0\x20\0", "image/gif"), Some((64,32)));
        assert_eq!(dimensions(b"\xff\xd8\xff\xc0\0\x0b\x08\0\x20\0\x40\x01\x01\x11\0", "image/jpeg"), Some((64,32)));
        let mut webp = b"RIFFxxxxWEBPVP8Xxxxx\0\0\0\0".to_vec();
        webp.extend([63,0,0,31,0,0]);
        assert_eq!(dimensions(&webp, "image/webp"), Some((64,32)));
        assert_eq!(dimensions(b"\xff\xd8\xff\xe0\xff\xff", "image/jpeg"), None);
        assert_eq!(pages(&json!({})).unwrap(), vec![1]);
        assert_eq!(pages(&json!({"pages":[2,2,1]})).unwrap(), vec![2, 1]);
        for args in [
            json!({"pages":[]}),
            json!({"pages":[0]}),
            json!({"pages":[1,2,3,4,5]}),
            json!({"pages":"1-3"}),
        ] {
            assert!(pages(&args).is_err());
        }
        assert!(image(&vec![0; MAX_IMAGE_BYTES + 1], "image/png").is_err());
        assert!(!validate_image_block(&json!({"type":"image","mimeType":"image/png","data":"not base64"})));
        assert!(!validate_image_block(&json!({"type":"image","mimeType":"image/jpeg","data":encode(b"\x89PNG\r\n\x1a\n")})));
    }

    fn fixture_pdf() -> Vec<u8> {
        let stream = "BT /F1 20 Tf 30 80 Td (Zen PDF fixture) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 240 120] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut pdf = "%PDF-1.4\n".to_string();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", i + 1));
        }
        let xref = pdf.len();
        pdf.push_str("xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets { pdf.push_str(&format!("{offset:010} 00000 n \n")); }
        pdf.push_str(&format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"));
        pdf.into_bytes()
    }

    #[tokio::test]
    async fn pdf_text_and_selected_page_are_rendered_in_containment() {
        if ["pdftotext", "pdftoppm"].iter().any(|program| std::process::Command::new(program).arg("-v").output().is_err()) {
            assert!(std::env::var_os("ZEN_TEST_REQUIRE_MEDIA").is_none(), "PDF tests require poppler-utils");
            eprintln!("PDF fixture skipped: install poppler-utils (CI installs it)");
            return;
        }
        let (text, blocks) = read(&fixture_pdf(), &json!({"pages":[1]})).await.unwrap().unwrap();
        assert!(text.contains("Zen PDF fixture"));
        assert_eq!(blocks[0]["text"], "PDF page 1");
        assert!(validate_image_block(&blocks[1]));
        assert!(read(&fixture_pdf(), &json!({"pages":[2]})).await.is_err());
    }

    #[tokio::test]
    async fn ordinary_text_is_left_to_reader_and_images_are_structured() {
        assert!(read(b"hello", &json!({})).await.unwrap().is_none());
        let (_, blocks) = read(&png_header(1,1), &json!({}))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(blocks[0]["mimeType"], "image/png");
        assert!(validate_image_block(&blocks[0]));
    }
}
