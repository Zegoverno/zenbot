//! stock: prints the warehouse stock levels.

struct Item {
    name: &'static str,
    count: u32,
}

fn items() -> Vec<Item> {
    vec![Item { name: "pallets", count: 12 }, Item { name: "crates", count: 40 }, Item { name: "drums", count: 3 }]
}

fn table(items: &[Item]) -> String {
    items.iter().map(|i| format!("{:<10}{:>5}", i.name, i.count)).collect::<Vec<_>>().join("\n")
}

fn main() {
    println!("{}", table(&items()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_one_line_per_item() {
        assert_eq!(table(&items()).lines().count(), 3);
    }
}
