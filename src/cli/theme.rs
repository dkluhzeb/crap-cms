//! Shared `ColorfulTheme` factory for all dialoguer prompts.

use console::style;
use dialoguer::theme::ColorfulTheme;

use super::glyphs;

/// Build the standard Crap CMS theme for all dialoguer prompts.
///
/// Uses `?` as the prompt prefix (cyan), `✓` for success (green), `✗` for errors (red).
/// Glyphs route through [`super::glyphs`] so the ASCII fallback
/// (`+` / `x`) kicks in when the terminal lacks Unicode support.
#[must_use]
pub fn crap_theme() -> ColorfulTheme {
    ColorfulTheme {
        prompt_prefix: style(glyphs::prompt().to_string()).cyan().bold(),
        success_prefix: style(glyphs::success().to_string()).green().bold(),
        error_prefix: style(glyphs::error().to_string()).red().bold(),
        ..ColorfulTheme::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_creates_without_panic() {
        let theme = crap_theme();
        // Verify the theme has the expected prefix strings
        assert!(theme.prompt_prefix.to_string().contains('?'));
        // success / error glyphs depend on terminal capability — verify
        // they're one of the documented variants.
        let success = theme.success_prefix.to_string();
        assert!(success.contains('✓') || success.contains('+'));
        let error = theme.error_prefix.to_string();
        assert!(error.contains('✗') || error.contains('x'));
    }
}
