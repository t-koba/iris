use chrono::{Datelike, Duration, Months, NaiveDate};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DateRangeSelection {
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
}

impl DateRangeSelection {
    pub fn normalized(self) -> Self {
        match (self.start, self.end) {
            (Some(start), Some(end)) if end < start => Self {
                start: Some(end),
                end: Some(start),
            },
            _ => self,
        }
    }

    pub fn contains(self, date: NaiveDate) -> bool {
        let selection = self.normalized();
        match (selection.start, selection.end) {
            (Some(start), Some(end)) => date >= start && date <= end,
            (Some(start), None) => date == start,
            _ => false,
        }
    }

    pub fn click(self, date: NaiveDate) -> Self {
        match (self.start, self.end) {
            (None, _) | (Some(_), Some(_)) => Self {
                start: Some(date),
                end: None,
            },
            (Some(start), None) if date < start => Self {
                start: Some(date),
                end: Some(start),
            },
            (Some(start), None) => Self {
                start: Some(start),
                end: Some(date),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonthGrid {
    pub year: i32,
    pub month: u32,
    pub cells: Vec<DayCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayCell {
    pub date: NaiveDate,
    pub in_month: bool,
    pub selected_start: bool,
    pub selected_end: bool,
    pub in_range: bool,
    pub has_data: bool,
}

pub fn two_month_grids(
    anchor: NaiveDate,
    selection: DateRangeSelection,
    marked_dates: &BTreeSet<NaiveDate>,
) -> [MonthGrid; 2] {
    let first = first_day_of_month(anchor);
    let second = first
        .checked_add_months(Months::new(1))
        .expect("next month should be representable");
    [
        month_grid(first, selection, marked_dates),
        month_grid(second, selection, marked_dates),
    ]
}

pub fn month_grid(
    month: NaiveDate,
    selection: DateRangeSelection,
    marked_dates: &BTreeSet<NaiveDate>,
) -> MonthGrid {
    let first = first_day_of_month(month);
    let weekday_offset = first.weekday().num_days_from_monday() as i64;
    let grid_start = first - Duration::days(weekday_offset);
    let selection = selection.normalized();
    let cells = (0..42)
        .map(|offset| {
            let date = grid_start + Duration::days(offset);
            DayCell {
                date,
                in_month: date.month() == first.month(),
                selected_start: selection.start == Some(date),
                selected_end: selection.end == Some(date),
                in_range: selection.contains(date),
                has_data: marked_dates.contains(&date),
            }
        })
        .collect();

    MonthGrid {
        year: first.year(),
        month: first.month(),
        cells,
    }
}

pub fn preset_today(today: NaiveDate) -> DateRangeSelection {
    DateRangeSelection {
        start: Some(today),
        end: Some(today),
    }
}

pub fn preset_month(today: NaiveDate) -> DateRangeSelection {
    let start = first_day_of_month(today);
    let next_month = start
        .checked_add_months(Months::new(1))
        .expect("next month should be representable");
    DateRangeSelection {
        start: Some(start),
        end: Some(next_month - Duration::days(1)),
    }
}

pub fn preset_year(today: NaiveDate) -> DateRangeSelection {
    let start = NaiveDate::from_ymd_opt(today.year(), 1, 1).expect("valid year start");
    let end = NaiveDate::from_ymd_opt(today.year(), 12, 31).expect("valid year end");
    DateRangeSelection {
        start: Some(start),
        end: Some(end),
    }
}

fn first_day_of_month(date: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(date.year(), date.month(), 1).expect("valid month")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        NaiveDate::parse_from_str(value, "%Y-%m-%d").expect("date")
    }

    #[test]
    fn click_selects_start_then_normalized_range() {
        let selection = DateRangeSelection::default().click(date("2024-03-20"));
        assert_eq!(selection.start, Some(date("2024-03-20")));
        assert_eq!(selection.end, None);

        let selection = selection.click(date("2024-03-10"));
        assert_eq!(selection.start, Some(date("2024-03-10")));
        assert_eq!(selection.end, Some(date("2024-03-20")));
        assert!(selection.contains(date("2024-03-15")));
    }

    #[test]
    fn month_grid_starts_on_monday_and_marks_data() {
        let marked = BTreeSet::from([date("2024-04-10")]);
        let grid = month_grid(
            date("2024-04-15"),
            DateRangeSelection {
                start: Some(date("2024-04-10")),
                end: Some(date("2024-04-12")),
            },
            &marked,
        );

        assert_eq!(grid.year, 2024);
        assert_eq!(grid.month, 4);
        assert_eq!(grid.cells.len(), 42);
        assert_eq!(grid.cells[0].date, date("2024-04-01"));
        let marked_cell = grid
            .cells
            .iter()
            .find(|cell| cell.date == date("2024-04-10"))
            .expect("marked day");
        assert!(marked_cell.has_data);
        assert!(marked_cell.selected_start);
        assert!(marked_cell.in_range);
    }

    #[test]
    fn two_month_grid_uses_anchor_month_and_next_month() {
        let grids = two_month_grids(
            date("2024-12-24"),
            DateRangeSelection::default(),
            &BTreeSet::new(),
        );
        assert_eq!((grids[0].year, grids[0].month), (2024, 12));
        assert_eq!((grids[1].year, grids[1].month), (2025, 1));
    }
}
