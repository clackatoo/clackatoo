//! Horizontal tab stops.

/// Columns where a horizontal tab stops.
#[derive(Clone, Debug)]
pub(crate) struct TabStops {
    stops: Vec<bool>,
}

impl TabStops {
    const INTERVAL: usize = 8;

    /// Stops every eight columns, the default since teletypes.
    pub(crate) fn new(width: usize) -> Self {
        Self {
            stops: (0..width).map(|col| col % Self::INTERVAL == 0).collect(),
        }
    }

    /// The first stop after `col`, or `None` if there is none before the edge.
    pub(crate) fn next(&self, col: usize) -> Option<usize> {
        (col + 1..self.stops.len()).find(|&c| self.stops[c])
    }

    /// The last stop before `col`, or `None` if there is none.
    pub(crate) fn previous(&self, col: usize) -> Option<usize> {
        (0..col.min(self.stops.len()))
            .rev()
            .find(|&c| self.stops[c])
    }

    pub(crate) fn set(&mut self, col: usize) {
        if let Some(stop) = self.stops.get_mut(col) {
            *stop = true;
        }
    }

    pub(crate) fn clear(&mut self, col: usize) {
        if let Some(stop) = self.stops.get_mut(col) {
            *stop = false;
        }
    }

    pub(crate) fn clear_all(&mut self) {
        self.stops.fill(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_stops_are_every_eight_columns() {
        let tabs = TabStops::new(20);
        assert_eq!(tabs.next(0), Some(8));
        assert_eq!(tabs.next(8), Some(16));
        assert_eq!(tabs.next(16), None);
        assert_eq!(tabs.previous(16), Some(8));
        assert_eq!(tabs.previous(0), None);
    }

    #[test]
    fn stops_can_be_set_and_cleared() {
        let mut tabs = TabStops::new(20);
        tabs.clear_all();
        assert_eq!(tabs.next(0), None);
        tabs.set(3);
        tabs.set(99);
        assert_eq!(tabs.next(0), Some(3));
        tabs.clear(3);
        assert_eq!(tabs.next(0), None);
    }
}
