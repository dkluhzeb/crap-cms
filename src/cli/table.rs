//! Simple CLI table renderer with auto-calculated column widths.

use console::style;

/// A simple table for CLI output with bold headers and auto-calculated column widths.
pub struct Table {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Create a new table with the given column headers.
    pub fn new(headers: Vec<&str>) -> Self {
        Self {
            headers: headers.into_iter().map(|h| h.to_string()).collect(),
            rows: Vec::new(),
        }
    }

    /// Add a row. The number of cells should match the header count.
    pub fn row(&mut self, cells: Vec<&str>) {
        self.rows
            .push(cells.into_iter().map(|c| c.to_string()).collect());
    }

    /// Calculate column widths based on content (headers + rows).
    fn column_widths(&self) -> Vec<usize> {
        let col_count = self.headers.len();
        let mut widths: Vec<usize> = self.headers.iter().map(|h| h.len()).collect();

        for row in &self.rows {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count && cell.len() > widths[i] {
                    widths[i] = cell.len();
                }
            }
        }

        // Add padding (2 spaces between columns)
        for w in &mut widths {
            *w += 2;
        }

        widths
    }

    /// Print the table to stdout: header, separator, data rows.
    pub fn print(&self) {
        let widths = self.column_widths();

        self.print_header(&widths);
        self.print_separator(&widths);
        self.print_rows(&widths);
    }

    /// Print the bold header row.
    fn print_header(&self, widths: &[usize]) {
        let line: String = self
            .headers
            .iter()
            .enumerate()
            .map(|(i, h)| {
                let w = widths.get(i).copied().unwrap_or(h.len() + 2);
                format!("{}", style(format!("{:<width$}", h, width = w)).bold())
            })
            .collect::<Vec<_>>()
            .join("");

        println!("{}", line);
    }

    /// Print a dimmed horizontal separator line.
    fn print_separator(&self, widths: &[usize]) {
        let total_width: usize = widths.iter().sum();

        println!("{}", style("─".repeat(total_width)).dim());
    }

    /// Print all data rows with aligned columns.
    fn print_rows(&self, widths: &[usize]) {
        for row in &self.rows {
            let line: String = row
                .iter()
                .enumerate()
                .map(|(i, cell)| {
                    let w = widths.get(i).copied().unwrap_or(cell.len() + 2);
                    format!("{:<width$}", cell, width = w)
                })
                .collect::<Vec<_>>()
                .join("");

            println!("{}", line);
        }
    }

    /// Print a footer line (e.g., row count) after the table.
    pub fn footer(&self, msg: &str) {
        println!("\n{}", style(msg).dim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_new_and_print_no_panic() {
        let mut table = Table::new(vec!["ID", "Name", "Status"]);
        table.row(vec!["abc123", "Alice", "active"]);
        table.row(vec!["def456", "Bob", "locked"]);
        table.print();
        table.footer("2 user(s)");
    }

    #[test]
    fn column_widths_respect_content() {
        let mut table = Table::new(vec!["ID", "Name"]);
        table.row(vec!["a-very-long-id-string", "A"]);
        let widths = table.column_widths();
        // Widths should be at least as wide as the longest content + padding
        assert!(widths[0] >= "a-very-long-id-string".len() + 2);
        assert!(widths[1] >= "Name".len() + 2);
    }

    #[test]
    fn empty_table_prints_headers_only() {
        let table = Table::new(vec!["Col1", "Col2"]);
        table.print(); // Should not panic
    }

    #[test]
    fn table_with_mismatched_row_length() {
        let mut table = Table::new(vec!["A", "B", "C"]);
        table.row(vec!["1", "2"]); // Fewer cells than headers
        table.print(); // Should not panic
    }
}
