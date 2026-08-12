//! Development tasks for remarkable-calendar-notes.
//!
//! `cargo run -p xtask -- icon` (re)generates `assets/icon.png`
//! deterministically: a small calendar-page glyph with a handwritten mark,
//! drawn entirely with programmatic shapes (no external image assets, no
//! third-party artwork), so the icon can always be reproduced or restyled
//! without needing an image editor.

use std::env;
use std::path::PathBuf;

const SIZE: usize = 256;

#[derive(Clone, Copy)]
struct Rgba(u8, u8, u8, u8);

const WHITE: Rgba = Rgba(255, 255, 255, 255);
const INK_BLACK: Rgba = Rgba(30, 30, 30, 255);
const HEADER_RED: Rgba = Rgba(196, 58, 58, 255);
const GRID_GRAY: Rgba = Rgba(150, 150, 150, 255);
const PEN_BLUE: Rgba = Rgba(40, 90, 200, 255);
const TRANSPARENT: Rgba = Rgba(0, 0, 0, 0);

struct Canvas {
    pixels: Vec<Rgba>,
}

impl Canvas {
    fn new() -> Self {
        Canvas {
            pixels: vec![TRANSPARENT; SIZE * SIZE],
        }
    }

    fn set(&mut self, x: i64, y: i64, color: Rgba) {
        if x < 0 || y < 0 || x as usize >= SIZE || y as usize >= SIZE {
            return;
        }
        self.pixels[y as usize * SIZE + x as usize] = color;
    }

    fn fill_rect(&mut self, x0: i64, y0: i64, w: i64, h: i64, color: Rgba) {
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                self.set(x, y, color);
            }
        }
    }

    fn stroke_rect(&mut self, x0: i64, y0: i64, w: i64, h: i64, thickness: i64, color: Rgba) {
        self.fill_rect(x0, y0, w, thickness, color);
        self.fill_rect(x0, y0 + h - thickness, w, thickness, color);
        self.fill_rect(x0, y0, thickness, h, color);
        self.fill_rect(x0 + w - thickness, y0, thickness, h, color);
    }

    fn fill_circle(&mut self, cx: i64, cy: i64, r: i64, color: Rgba) {
        for y in -r..=r {
            for x in -r..=r {
                if x * x + y * y <= r * r {
                    self.set(cx + x, cy + y, color);
                }
            }
        }
    }

    fn draw_line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64, thickness: i64, color: Rgba) {
        let dx = (x1 - x0).abs();
        let dy = -(y1 - y0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        let (mut x, mut y) = (x0, y0);
        let half = thickness / 2;
        loop {
            for oy in -half..=half {
                for ox in -half..=half {
                    self.set(x + ox, y + oy, color);
                }
            }
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    }
}

fn build_icon() -> Canvas {
    let mut c = Canvas::new();

    // Page body.
    let (px, py, pw, ph) = (24, 30, SIZE as i64 - 48, SIZE as i64 - 54);
    c.fill_rect(px, py, pw, ph, WHITE);
    c.stroke_rect(px, py, pw, ph, 6, INK_BLACK);

    // Header bar (the little colored strip real calendar-app icons use).
    let header_h = 34;
    c.fill_rect(px + 3, py + 3, pw - 6, header_h, HEADER_RED);

    // Binder rings.
    c.fill_circle(px + pw / 3, py, 10, INK_BLACK);
    c.fill_circle(px + 2 * pw / 3, py, 10, INK_BLACK);

    // A 4-column x 3-row day grid below the header.
    let grid_top = py + header_h + 12;
    let grid_bottom = py + ph - 14;
    let cols = 4;
    let rows = 3;
    let col_w = (pw - 20) / cols;
    let row_h = (grid_bottom - grid_top) / rows;
    for col in 0..=cols {
        let x = px + 10 + col * col_w;
        c.fill_rect(x, grid_top, 2, grid_bottom - grid_top, GRID_GRAY);
    }
    for row in 0..=rows {
        let y = grid_top + row * row_h;
        c.fill_rect(px + 10, y, pw - 20, 2, GRID_GRAY);
    }

    // A short handwritten-looking mark across two cells, signaling
    // "notes" — this app's defining feature over a plain calendar.
    let mark_y = grid_top + row_h + row_h / 2;
    c.draw_line(
        px + 10 + col_w / 2,
        mark_y - 10,
        px + 10 + col_w + col_w / 3,
        mark_y + 8,
        6,
        PEN_BLUE,
    );
    c.draw_line(
        px + 10 + col_w + col_w / 3,
        mark_y + 8,
        px + 10 + 2 * col_w + col_w / 4,
        mark_y - 6,
        6,
        PEN_BLUE,
    );

    c
}

fn write_png(canvas: &Canvas, path: &PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, SIZE as u32, SIZE as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut data = Vec::with_capacity(SIZE * SIZE * 4);
    for p in &canvas.pixels {
        data.extend_from_slice(&[p.0, p.1, p.2, p.3]);
    }
    writer
        .write_image_data(&data)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("icon") => {
            let out = repo_root().join("assets").join("icon.png");
            let canvas = build_icon();
            match write_png(&canvas, &out) {
                Ok(()) => {
                    println!("wrote {}", out.display());
                    std::process::ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("failed to write {}: {e}", out.display());
                    std::process::ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: cargo run -p xtask -- icon");
            std::process::ExitCode::FAILURE
        }
    }
}

fn repo_root() -> PathBuf {
    // xtask's own crate dir is <repo>/xtask, so its parent is the repo root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

/// Repository-shape checks that need no device, network, or Docker: they
/// keep the packaging metadata (Vellum recipe, AppLoad manifest, release
/// workflow) consistent with each other, and — importantly — assert that
/// the recipe's checksum placeholders stay recognizably *invalid* until a
/// real release has been published. They deliberately do not pretend a
/// placeholder is a checksum.
#[cfg(test)]
mod packaging_tests {
    use super::repo_root;
    use std::fs;

    const CHECKSUM_PLACEHOLDER: &str = "PLACEHOLDER-";

    fn velbuild() -> String {
        fs::read_to_string(
            repo_root()
                .join("vellum")
                .join("packages")
                .join("remarkable-calendar-notes")
                .join("VELBUILD"),
        )
        .expect("the Vellum recipe exists")
    }

    fn sidebar_velbuild() -> String {
        fs::read_to_string(
            repo_root()
                .join("vellum")
                .join("packages")
                .join("remarkable-calendar-notes-sidebar")
                .join("VELBUILD"),
        )
        .expect("the sidebar Vellum recipe exists")
    }

    fn field<'a>(text: &'a str, name: &str) -> &'a str {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{name}=")))
            .unwrap_or_else(|| panic!("VELBUILD has no {name}= field"))
            .trim_matches('"')
    }

    fn workspace_version() -> String {
        let manifest = fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
        manifest
            .lines()
            .find_map(|l| l.strip_prefix("version = "))
            .expect("workspace version")
            .trim()
            .trim_matches('"')
            .to_string()
    }

    #[test]
    fn velbuild_targets_rm2_only_via_device_exclusions() {
        let text = velbuild();
        let depends = field(&text, "depends");
        for excluded in ["!rm1", "!rmpp", "!rmppm", "!rmppure"] {
            assert!(
                depends.split_whitespace().any(|d| d == excluded),
                "depends= must exclude {excluded}: {depends}"
            );
        }
        assert!(depends.contains("appload>=0.5.3"), "{depends}");
        assert!(depends.contains("remarkable-os>=3.26"), "{depends}");
        assert!(depends.contains("remarkable-os<3.28"), "{depends}");
    }

    #[test]
    fn velbuild_checksums_are_explicit_placeholders_not_fake_digests() {
        let text = velbuild();
        let block = text
            .split("sha512sums=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a sha512sums block");
        for line in block.lines().filter(|l| !l.trim().is_empty()) {
            let digest = line.split_whitespace().next().unwrap();
            if digest.starts_with(CHECKSUM_PLACEHOLDER) {
                continue; // pre-release: honestly marked as not-a-checksum
            }
            // Post-release, the only other acceptable form is a real
            // 128-hex-character sha512 digest.
            assert_eq!(
                digest.len(),
                128,
                "checksum is neither a placeholder nor a sha512 digest: {digest}"
            );
            assert!(
                digest.chars().all(|c| c.is_ascii_hexdigit()),
                "checksum is not hexadecimal: {digest}"
            );
            assert!(
                digest.chars().any(|c| c != '0'),
                "an all-zero digest is not a valid checksum: {digest}"
            );
        }
    }

    #[test]
    fn velbuild_source_and_release_workflow_agree_on_the_archive_name() {
        let text = velbuild();
        let pkgver = field(&text, "pkgver");
        let archive = format!("remarkable-calendar-notes-{pkgver}-armv7.zip");
        let templated = archive.replace(pkgver, "$pkgver");

        // The recipe downloads exactly what the release workflow uploads...
        assert!(
            text.contains(&format!("/releases/download/v$pkgver/{templated}")),
            "source= must point at the release asset for v$pkgver"
        );
        assert!(
            text.contains("github.com/$upstream_author/remarkable-calendar-notes/releases"),
            "Vellum must fetch from the main repository's public releases"
        );

        let workflow =
            fs::read_to_string(repo_root().join(".github/workflows/release.yml")).unwrap();
        assert!(
            workflow.contains("dist/remarkable-calendar-notes-${version}-armv7.zip"),
            "release.yml must build the archive name the recipe expects"
        );
        // ...and the archive's single top-level directory is what the
        // recipe's unpack()/package() steps reach into.
        assert!(workflow.contains("dist/stage/remarkable-calendar-notes"));
        assert!(workflow
            .contains(r#"(cd dist/stage && zip -r -X "../../$out" remarkable-calendar-notes)"#));
        assert!(text.contains("\"$srcdir\"/remarkable-calendar-notes/remarkable-calendar-notes"));
        assert!(text.contains("\"$srcdir\"/remarkable-calendar-notes/icon.png"));
        assert!(text.contains("\"$srcdir\"/remarkable-calendar-notes/external.manifest.json"));
    }

    #[test]
    fn release_workflow_uploads_the_archive_and_its_checksums() {
        let workflow =
            fs::read_to_string(repo_root().join(".github/workflows/release.yml")).unwrap();
        for line in ["sha256sum \"$out\"", "sha512sum \"$out\""] {
            assert!(workflow.contains(line), "release.yml must emit {line}");
        }
        // The publish job downloads the artifact into dist/ and uploads
        // exactly those globs.
        assert!(workflow.contains("path: dist"));
        for glob in ["dist/*.zip", "dist/*.sha256", "dist/*.sha512"] {
            assert!(workflow.contains(glob), "release.yml must upload {glob}");
        }
        assert!(!workflow.contains("remarkable-calendar-notes-releases"));
        assert!(!workflow.contains("secrets.PUBLIC_RELEASE_TOKEN"));
    }

    #[test]
    fn versions_are_consistent_across_manifest_recipe_and_workspace() {
        let version = workspace_version();
        let manifest = fs::read_to_string(repo_root().join("external.manifest.json")).unwrap();
        assert!(
            manifest.contains(&format!("\"version\": \"{version}\"")),
            "external.manifest.json version must match the workspace version {version}"
        );
        let text = velbuild();
        assert_eq!(field(&text, "pkgver"), version);
        assert_eq!(field(&sidebar_velbuild(), "pkgver"), version);
    }

    #[test]
    fn sidebar_package_is_firmware_pinned_and_launches_the_external_app() {
        let recipe = sidebar_velbuild();
        let depends = field(&recipe, "depends");
        for dependency in [
            "remarkable-calendar-notes",
            "qt-resource-rebuilder",
            "appload>=0.5.3",
            "remarkable-os>=3.27",
            "remarkable-os<3.28",
        ] {
            assert!(depends.contains(dependency), "{depends}");
        }
        let qmd =
            fs::read_to_string(repo_root().join("sidebar/3.27/calendarNotesSidebar.qmd")).unwrap();
        assert!(qmd.contains("SPDX-License-Identifier: GPL-3.0-only"));
        assert!(qmd
            .contains("AppLoadLauncher.launchApplication(\"external::remarkable-calendar-notes\""));
    }

    #[test]
    fn release_workflow_builds_the_optional_sidebar_archive() {
        let workflow =
            fs::read_to_string(repo_root().join(".github/workflows/release.yml")).unwrap();
        assert!(workflow.contains("dist/remarkable-calendar-notes-${version}-xovi-sidebar.zip"));
        assert!(workflow.contains("sidebar/3.27/calendarNotesSidebar.qmd"));
        assert!(workflow.contains("remarkable-calendar-notes-xovi-sidebar/qt-resource-rebuilder"));
    }

    #[test]
    fn checksum_entries_name_exactly_the_source_files() {
        let text = velbuild();
        let pkgver = field(&text, "pkgver");
        let sources: Vec<String> = text
            .split("source=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a source block")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                l.trim()
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .replace("$pkgver", pkgver)
            })
            .collect();
        let checksums: Vec<String> = text
            .split("sha512sums=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a sha512sums block")
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.split_whitespace().nth(1).unwrap().to_string())
            .collect();
        assert_eq!(
            sources, checksums,
            "every source= entry needs a matching sha512sums line, in order"
        );
    }
}
