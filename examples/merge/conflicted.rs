//! A small inventory, for trying Nimbus's compare and merge views.

use std::collections::HashMap;

/// One thing on the shelf.
#[derive(Debug, Clone)]
pub struct Item {
    pub name: String,
    pub count: u32,
    pub price_cents: u32,
    /// Where it sits, if anyone wrote it down.
    pub shelf: Option<String>,
}

/// Items by name.
#[derive(Default)]
pub struct Inventory {
    items: HashMap<String, Item>,
}

impl Inventory {
    pub fn new() -> Self {
        Self {
            items: HashMap::new(),
        }
    }

    /// Put `count` more of `name` on the shelf at `price_cents` each.
    pub fn stock(&mut self, name: &str, count: u32, price_cents: u32) {
        let item = self.items.entry(name.to_owned()).or_insert(Item {
            name: name.to_owned(),
            count: 0,
            price_cents,
            shelf: None,
        });
        item.count += count;
        item.price_cents = price_cents;
    }

    /// Take `count` of `name`; `false` when there are not enough.
    pub fn take(&mut self, name: &str, count: u32) -> bool {
        match self.items.get_mut(name) {
            Some(item) if item.count >= count => {
                item.count -= count;
                true
            }
<<<<<<< local
            Some(_) => false,
            None => {
                eprintln!("no such item: {name}");
                false
            }
=======
            Some(_) | None => false,
>>>>>>> remote
        }
    }

    /// The value of everything on the shelf, in cents.
    pub fn total_cents(&self) -> u64 {
        self.items
            .values()
            .map(|i| u64::from(i.count) * u64::from(i.price_cents))
            .sum::<u64>()
    }

    /// How many distinct things are stocked.
    pub fn kinds(&self) -> usize {
        self.items.len()
    }

    /// Everything that has run out.
    pub fn empty(&self) -> impl Iterator<Item = &Item> {
        self.items.values().filter(|i| i.count == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stock_and_take() {
        let mut inv = Inventory::new();
        inv.stock("apple", 3, 50);
        assert!(inv.take("apple", 2));
        assert!(!inv.take("apple", 2));
        assert_eq!(inv.total_cents(), 50);
    }

    #[test]
    fn kinds_counts_names_once() {
        let mut inv = Inventory::new();
        inv.stock("pear", 1, 10);
        inv.stock("pear", 1, 10);
        assert_eq!(inv.kinds(), 1);
    }
}
