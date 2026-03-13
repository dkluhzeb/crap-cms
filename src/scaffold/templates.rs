//! Template/static file listing, extraction, and proto export.

use anyhow::{Context as _, Result, bail};
use std::{fs, io::Write, path::Path};

use include_dir::{Dir, include_dir};

/// Embedded default templates — compiled into the binary.
static EMBEDDED_TEMPLATES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/templates");
/// Embedded default static files — compiled into the binary.
static EMBEDDED_STATIC: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/static");

/// The embedded proto file content — compiled into the binary.
const PROTO_CONTENT: &str = include_str!("../../proto/content.proto");

/// Recursively collect all files from an `include_dir::Dir`, returning `(relative_path, content)`.
/// Paths are relative to the root Dir.
fn collect_embedded_files_flat<'a>(dir: &'a Dir<'a>) -> Vec<(String, &'a [u8])> {
    let mut out = Vec::new();
    for file in dir.files() {
        out.push((file.path().to_string_lossy().to_string(), file.contents()));
    }
    for sub in dir.dirs() {
        out.extend(collect_embedded_files_flat(sub));
    }
    out
}

/// Format a file size as human-readable (e.g., "1.2 KB", "92.0 KB").
fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Functional category for a template or static file.
struct FileCategory {
    label: &'static str,
    description: &'static str,
    /// Prefix patterns to match (e.g., "layout/" matches "layout/base.hbs").
    prefixes: &'static [&'static str],
}

/// Template categories ordered by customization relevance.
const TEMPLATE_CATEGORIES: &[FileCategory] = &[
    FileCategory {
        label: "Layout",
        description: "Page shell, header, sidebar navigation",
        prefixes: &["layout/"],
    },
    FileCategory {
        label: "Fields",
        description: "Form input partials (text, select, checkbox, richtext, ...)",
        prefixes: &["fields/"],
    },
    FileCategory {
        label: "Collections",
        description: "List, create, edit pages and table rows",
        prefixes: &["collections/"],
    },
    FileCategory {
        label: "Globals",
        description: "Global settings edit pages",
        prefixes: &["globals/"],
    },
    FileCategory {
        label: "Auth",
        description: "Login, forgot password, reset password pages",
        prefixes: &["auth/"],
    },
    FileCategory {
        label: "Dashboard",
        description: "Admin landing page",
        prefixes: &["dashboard/"],
    },
    FileCategory {
        label: "Errors",
        description: "404, 403, 500 error pages",
        prefixes: &["errors/"],
    },
    FileCategory {
        label: "Email",
        description: "Password reset and email verification templates",
        prefixes: &["email/"],
    },
    FileCategory {
        label: "Components",
        description: "Breadcrumb, pagination, version history partials",
        prefixes: &["components/"],
    },
];

/// Static file categories.
const STATIC_CATEGORIES: &[FileCategory] = &[
    FileCategory {
        label: "Styles",
        description: "CSS files (design tokens, layout, forms, buttons, themes)",
        prefixes: &[".css"],
    },
    FileCategory {
        label: "Components",
        description: "JS modules (toast, confirm dialog, richtext editor, ...)",
        prefixes: &["components/"],
    },
    FileCategory {
        label: "Fonts",
        description: "Geist font family (woff2/otf/ttf)",
        prefixes: &["fonts/"],
    },
];

/// Print files grouped by functional category with descriptions.
fn print_categorized(files: &[(String, &[u8])], categories: &[FileCategory]) {
    // Track which file indices are categorized
    let mut used = vec![false; files.len()];

    for cat in categories {
        let matched: Vec<usize> = files
            .iter()
            .enumerate()
            .filter(|(i, (path, _))| {
                !used[*i]
                    && cat.prefixes.iter().any(|prefix| {
                        if prefix.starts_with('.') {
                            path.ends_with(prefix) && !path.contains('/')
                        } else {
                            path.starts_with(prefix)
                        }
                    })
            })
            .map(|(i, _)| i)
            .collect();

        if matched.is_empty() {
            continue;
        }

        let total: usize = matched.iter().map(|&i| files[i].1.len()).sum();
        let n = matched.len();
        println!(
            "  {} ({} {}, {}) — {}",
            cat.label,
            n,
            if n == 1 { "file" } else { "files" },
            format_size(total),
            cat.description
        );
        for &i in &matched {
            println!("    {}", files[i].0);
            used[i] = true;
        }
        println!();
    }

    // Any uncategorized files
    let remaining: Vec<usize> = (0..files.len()).filter(|i| !used[*i]).collect();

    if !remaining.is_empty() {
        let total: usize = remaining.iter().map(|&i| files[i].1.len()).sum();
        let n = remaining.len();
        println!(
            "  Other ({} {}, {})",
            n,
            if n == 1 { "file" } else { "files" },
            format_size(total)
        );
        for &i in &remaining {
            println!("    {}", files[i].0);
        }
        println!();
    }
}

/// Print files grouped by directory in a tree-like format (verbose mode).
fn print_file_tree(files: &[(String, &[u8])]) {
    use std::collections::BTreeMap;

    // Group files by directory
    let mut dirs: BTreeMap<String, Vec<(&str, usize)>> = BTreeMap::new();
    for (path, content) in files {
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path.as_str()),
        };
        dirs.entry(dir.to_string())
            .or_default()
            .push((name, content.len()));
    }

    for (dir, entries) in &dirs {
        if !dir.is_empty() {
            println!("  {}/", dir);
        }
        for (name, size) in entries {
            let indent = if dir.is_empty() { "  " } else { "    " };
            println!("{}{:<40} {}", indent, name, format_size(*size));
        }
    }
}

/// List embedded templates and/or static files.
///
/// `type_filter`: None = both, Some("templates") = templates only, Some("static") = static only.
/// `verbose`: show full file tree instead of compact summary.
pub fn templates_list(type_filter: Option<&str>, verbose: bool) -> Result<()> {
    // Validate filter
    if let Some(f) = type_filter
        && f != "templates"
        && f != "static"
    {
        bail!("Invalid --type '{}' — valid: templates, static", f);
    }

    let show_templates = type_filter.is_none() || type_filter == Some("templates");
    let show_static = type_filter.is_none() || type_filter == Some("static");

    if show_templates {
        let files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
        let total_size: usize = files.iter().map(|(_, c)| c.len()).sum();
        println!(
            "Templates ({} files, {}):",
            files.len(),
            format_size(total_size)
        );

        if verbose {
            print_file_tree(&files);
        } else {
            println!();
            print_categorized(&files, TEMPLATE_CATEGORIES);
        }
        if show_static && !verbose {
            // categorized mode already adds spacing
        } else if show_static {
            println!();
        }
    }

    if show_static {
        let files = collect_embedded_files_flat(&EMBEDDED_STATIC);
        let total_size: usize = files.iter().map(|(_, c)| c.len()).sum();
        println!(
            "Static files ({} files, {}):",
            files.len(),
            format_size(total_size)
        );

        if verbose {
            print_file_tree(&files);
        } else {
            println!();
            print_categorized(&files, STATIC_CATEGORIES);
        }
    }

    if !verbose {
        println!("Extract a file to customize it:");
        println!("  crap-cms templates extract <CONFIG> <PATH>");
        println!("  crap-cms templates extract <CONFIG> --all");
    }

    Ok(())
}

/// Extract embedded templates/static files into a config directory.
///
/// `paths`: specific files to extract (e.g., "layout/base.hbs", "styles.css").
/// `all`: extract all files.
/// `type_filter`: None = both, Some("templates") = templates only, Some("static") = static only.
/// `force`: overwrite existing files.
pub fn templates_extract(
    config_dir: &Path,
    paths: &[String],
    all: bool,
    type_filter: Option<&str>,
    force: bool,
) -> Result<()> {
    // Validate filter
    if let Some(f) = type_filter
        && f != "templates"
        && f != "static"
    {
        bail!("Invalid --type '{}' — valid: templates, static", f);
    }

    if !all && paths.is_empty() {
        bail!("Specify file paths to extract, or use --all to extract everything");
    }

    let want_templates = type_filter.is_none() || type_filter == Some("templates");
    let want_static = type_filter.is_none() || type_filter == Some("static");

    if all {
        let mut count = 0usize;

        if want_templates {
            let files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
            for (path, content) in &files {
                let dest = config_dir.join("templates").join(path);

                if dest.exists() && !force {
                    println!("  Skipped: templates/{} (exists, use --force)", path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, content)?;
                count += 1;
            }
            if want_templates && !want_static {
                println!(
                    "Extracted {} template file(s) to {}/templates/",
                    count,
                    config_dir.display()
                );

                return Ok(());
            }
        }

        if want_static {
            let tpl_count = count;
            let files = collect_embedded_files_flat(&EMBEDDED_STATIC);
            for (path, content) in &files {
                let dest = config_dir.join("static").join(path);

                if dest.exists() && !force {
                    println!("  Skipped: static/{} (exists, use --force)", path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, content)?;
                count += 1;
            }
            if !want_templates {
                println!(
                    "Extracted {} static file(s) to {}/static/",
                    count,
                    config_dir.display()
                );

                return Ok(());
            }
            println!(
                "Extracted {} file(s) ({} templates, {} static) to {}/",
                count,
                tpl_count,
                count - tpl_count,
                config_dir.display()
            );
        }

        return Ok(());
    }

    // Extract specific paths
    let mut extracted = 0usize;
    for path in paths {
        // Try templates first, then static
        let found = if want_templates {
            EMBEDDED_TEMPLATES.get_file(path).map(|f| ("templates", f))
        } else {
            None
        };
        let found = found.or_else(|| {
            if want_static {
                EMBEDDED_STATIC.get_file(path).map(|f| ("static", f))
            } else {
                None
            }
        });

        match found {
            Some((kind, file)) => {
                let dest = config_dir.join(kind).join(path);

                if dest.exists() && !force {
                    println!("  Skipped: {}/{} (exists, use --force)", kind, path);
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&dest, file.contents())?;
                println!("  \u{2713} {}/{}", kind, path);
                extracted += 1;
            }
            None => {
                println!("  Not found: {}", path);
            }
        }
    }

    if extracted > 0 {
        println!(
            "Extracted {} file(s) to {}/",
            extracted,
            config_dir.display()
        );
    }

    Ok(())
}

/// Export the embedded `content.proto` file for gRPC client codegen.
///
/// - No `output` → writes to stdout (pipe-friendly).
/// - `output` is a directory → writes `content.proto` into it.
/// - `output` is a file path → writes directly to that file.
pub fn proto_export(output: Option<&Path>) -> Result<()> {
    match output {
        None => {
            // Write to stdout
            std::io::stdout()
                .write_all(PROTO_CONTENT.as_bytes())
                .context("Failed to write proto to stdout")?;
        }
        Some(path) => {
            let target = if path.is_dir() || path.to_string_lossy().ends_with('/') {
                fs::create_dir_all(path)
                    .with_context(|| format!("Failed to create directory '{}'", path.display()))?;
                path.join("content.proto")
            } else {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).with_context(|| {
                        format!("Failed to create directory '{}'", parent.display())
                    })?;
                }
                path.to_path_buf()
            };
            fs::write(&target, PROTO_CONTENT)
                .with_context(|| format!("Failed to write {}", target.display()))?;
            println!("Wrote {}", target.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_templates_list() {
        // Summary mode (default)
        assert!(templates_list(None, false).is_ok());
        assert!(templates_list(Some("templates"), false).is_ok());
        assert!(templates_list(Some("static"), false).is_ok());
        assert!(templates_list(Some("invalid"), false).is_err());
    }

    #[test]
    fn test_templates_list_verbose() {
        // Verbose mode (full file tree)
        assert!(templates_list(None, true).is_ok());
        assert!(templates_list(Some("templates"), true).is_ok());
        assert!(templates_list(Some("static"), true).is_ok());
        assert!(templates_list(Some("invalid"), true).is_err());
    }

    #[test]
    fn test_templates_list_has_files() {
        // Verify embedded dirs actually contain files
        let tpl_files = collect_embedded_files_flat(&EMBEDDED_TEMPLATES);
        assert!(!tpl_files.is_empty(), "should have embedded templates");
        assert!(tpl_files.iter().any(|(p, _)| p.ends_with(".hbs")));

        let static_files = collect_embedded_files_flat(&EMBEDDED_STATIC);
        assert!(
            !static_files.is_empty(),
            "should have embedded static files"
        );
        assert!(static_files.iter().any(|(p, _)| p.ends_with(".css")));
    }

    #[test]
    fn test_templates_extract_specific() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false,
            None,
            false,
        )
        .unwrap();

        assert!(tmp.path().join("templates/layout/base.hbs").exists());
        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert!(!content.is_empty());
    }

    #[test]
    fn test_templates_extract_static_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &["styles.css".to_string()], false, None, false).unwrap();

        assert!(tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_skips_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Extract once
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false,
            None,
            false,
        )
        .unwrap();

        // Write a marker to verify it doesn't get overwritten
        fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

        // Extract again without --force
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false,
            None,
            false,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert_eq!(content, "CUSTOM", "should not overwrite without --force");
    }

    #[test]
    fn test_templates_extract_force_overwrites() {
        let tmp = tempfile::tempdir().expect("tempdir");

        // Extract once
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false,
            None,
            false,
        )
        .unwrap();

        // Write a marker
        fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();

        // Extract again with --force
        templates_extract(
            tmp.path(),
            &["layout/base.hbs".to_string()],
            false,
            None,
            true,
        )
        .unwrap();

        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert_ne!(content, "CUSTOM", "should overwrite with --force");
    }

    #[test]
    fn test_templates_extract_all_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &[], true, Some("templates"), false).unwrap();

        // Should have created template files
        assert!(tmp.path().join("templates/layout/base.hbs").exists());
        // Should NOT have created static files
        assert!(!tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_all_static() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &[], true, Some("static"), false).unwrap();

        // Should have created static files
        assert!(tmp.path().join("static/styles.css").exists());
        // Should NOT have created template files
        assert!(!tmp.path().join("templates/layout/base.hbs").exists());
    }

    #[test]
    fn test_templates_extract_requires_paths_or_all() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = templates_extract(tmp.path(), &[], false, None, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--all"));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(100), "100 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 100), "100.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 5), "5.0 MB");
    }

    #[test]
    fn test_templates_extract_all_both() {
        let tmp = tempfile::tempdir().expect("tempdir");
        templates_extract(tmp.path(), &[], true, None, false).unwrap();

        // Should have created both template and static files
        assert!(tmp.path().join("templates/layout/base.hbs").exists());
        assert!(tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_all_with_existing_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // First extraction
        templates_extract(tmp.path(), &[], true, Some("templates"), false).unwrap();
        // Write marker
        fs::write(tmp.path().join("templates/layout/base.hbs"), "CUSTOM").unwrap();
        // Second extraction without force — should skip existing
        templates_extract(tmp.path(), &[], true, Some("templates"), false).unwrap();

        let content = fs::read_to_string(tmp.path().join("templates/layout/base.hbs")).unwrap();
        assert_eq!(
            content, "CUSTOM",
            "Should skip existing files without --force"
        );
    }

    #[test]
    fn test_templates_extract_all_static_with_existing_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // First extraction
        templates_extract(tmp.path(), &[], true, Some("static"), false).unwrap();
        // Write marker
        fs::write(tmp.path().join("static/styles.css"), "CUSTOM").unwrap();
        // Second extraction without force — should skip
        templates_extract(tmp.path(), &[], true, Some("static"), false).unwrap();
        let content = fs::read_to_string(tmp.path().join("static/styles.css")).unwrap();
        assert_eq!(content, "CUSTOM");
    }

    #[test]
    fn test_templates_extract_invalid_type() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let result = templates_extract(tmp.path(), &[], true, Some("invalid"), false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("--type"));
    }

    #[test]
    fn test_proto_export_to_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file_path = tmp.path().join("output.proto");
        proto_export(Some(&file_path)).unwrap();
        assert!(file_path.exists());
        let content = fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("syntax"));
    }

    #[test]
    fn test_proto_export_to_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("output_dir");
        fs::create_dir_all(&dir).unwrap();
        proto_export(Some(&dir)).unwrap();
        assert!(dir.join("content.proto").exists());
    }

    #[test]
    fn test_proto_export_to_stdout() {
        // Just verify it doesn't error
        proto_export(None).unwrap();
    }

    #[test]
    fn test_proto_export_to_nested_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file_path = tmp.path().join("nested/dir/output.proto");
        proto_export(Some(&file_path)).unwrap();
        assert!(file_path.exists());
    }

    #[test]
    fn test_templates_extract_specific_with_type_filter() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Extract only static files (styles.css should be found in static)
        templates_extract(
            tmp.path(),
            &["styles.css".to_string()],
            false,
            Some("static"),
            false,
        )
        .unwrap();

        assert!(tmp.path().join("static/styles.css").exists());
    }

    #[test]
    fn test_templates_extract_specific_skips_existing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // First extract
        templates_extract(tmp.path(), &["styles.css".to_string()], false, None, false).unwrap();
        // Write marker
        fs::write(tmp.path().join("static/styles.css"), "CUSTOM").unwrap();
        // Extract again without force — should skip
        templates_extract(tmp.path(), &["styles.css".to_string()], false, None, false).unwrap();
        let content = fs::read_to_string(tmp.path().join("static/styles.css")).unwrap();
        assert_eq!(content, "CUSTOM");
    }

    #[test]
    fn test_templates_extract_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Should not error, just print "Not found"
        templates_extract(
            tmp.path(),
            &["nonexistent/file.hbs".to_string()],
            false,
            None,
            false,
        )
        .unwrap();
    }
}
