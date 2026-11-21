use crate::{CampaignDay, Click, Event, Keys, UserDay};
use std::collections::HashMap;

const DAY: i64 = 86_400_000;
const WEEK: i64 = 7 * DAY;
const SESSION_GAP: i64 = 1_800_000;

#[derive(Clone, Copy, Default)]
struct Totals {
    sessions: i64,
    purchasing: i64,
    revenue: i64,
    assisted: i64,
}

// The two cursors delimit the inclusive seven-day window. Keep its distinct
// campaigns in a packed vector so sparse windows cost only their actual size.
struct ClickWindow {
    left: usize,
    right: usize,
    active: Vec<u32>,
    counts: Vec<usize>,
    positions: Vec<usize>,
}

impl ClickWindow {
    fn new(campaign_count: usize) -> Self {
        Self {
            left: 0,
            right: 0,
            active: Vec::new(),
            counts: vec![0; campaign_count],
            positions: vec![0; campaign_count],
        }
    }

    fn reset(&mut self) {
        for campaign in self.active.drain(..) {
            self.counts[campaign as usize] = 0;
        }
        self.left = 0;
        self.right = 0;
    }

    fn advance(&mut self, clicks: &[Click], start: i64) -> usize {
        let floor = start.saturating_sub(WEEK);
        while self.left < self.right && clicks[self.left].ts < floor {
            let campaign = clicks[self.left].campaign as usize;
            self.counts[campaign] -= 1;
            if self.counts[campaign] == 0 {
                let index = self.positions[campaign];
                self.active.swap_remove(index);
                if let Some(&moved) = self.active.get(index) {
                    self.positions[moved as usize] = index;
                }
            }
            self.left += 1;
        }

        // Sessions can be more than a week apart. Skip clicks that expired
        // between them without ever contributing to either window.
        if self.left == self.right {
            while self.right < clicks.len() && clicks[self.right].ts < floor {
                self.right += 1;
            }
            self.left = self.right;
        }
        while self.right < clicks.len() && clicks[self.right].ts <= start {
            let campaign = clicks[self.right].campaign as usize;
            if self.counts[campaign] == 0 {
                self.positions[campaign] = self.active.len();
                self.active.push(campaign as u32);
            }
            self.counts[campaign] += 1;
            self.right += 1;
        }
        if self.left == self.right {
            0
        } else {
            clicks[self.right - 1].campaign as usize
        }
    }
}

pub fn aggregate(
    mut events: Vec<Event>,
    mut clicks: Vec<Click>,
    keys: &Keys,
    campaign_count: usize,
) -> (Vec<UserDay>, Vec<CampaignDay>) {
    events.sort_unstable_by_key(|event| (event.person, event.ts));
    // Within a timestamp, the byte-wise smallest click ID goes last. Exact
    // duplicate clicks expire together, leave the winner unchanged, and share
    // one active campaign entry, so deduplicating them here is unnecessary.
    clicks.sort_unstable_by(|a, b| {
        a.person
            .cmp(&b.person)
            .then_with(|| a.ts.cmp(&b.ts))
            .then_with(|| keys.cmp(b.id, a.id))
    });

    let mut user_rows = Vec::new();
    let mut campaign_days: HashMap<i32, Vec<Totals>> = HashMap::new();
    let mut categories: Vec<(u32, i64)> = Vec::new();
    let mut window = ClickWindow::new(campaign_count);
    let mut event_index = 0;
    let mut click_index = 0;
    while event_index < events.len() {
        let person = events[event_index].person;
        while click_index < clicks.len() && clicks[click_index].person < person {
            click_index += 1;
        }
        let first_click = click_index;
        while click_index < clicks.len() && clicks[click_index].person == person {
            click_index += 1;
        }
        let person_clicks = &clicks[first_click..click_index];
        window.reset();

        while event_index < events.len() && events[event_index].person == person {
            let day = events[event_index].ts.div_euclid(DAY) as i32;
            let mut daily = UserDay {
                person,
                day,
                sessions: 0,
                events: 0,
                revenue: 0,
                top: None,
            };
            let totals = campaign_days
                .entry(day)
                .or_insert_with(|| vec![Totals::default(); campaign_count]);
            categories.clear();
            loop {
                let start = events[event_index].ts;
                let credited = window.advance(person_clicks, start);
                for &campaign in &window.active {
                    totals[campaign as usize].assisted += 1;
                }

                let first_event = event_index;
                let mut revenue: i64 = 0;
                loop {
                    let event = &events[event_index];
                    if event.price > 0 {
                        let price = i64::from(event.price);
                        revenue += price;
                        if let Some((_, sum)) =
                            categories.iter_mut().find(|(id, _)| *id == event.category)
                        {
                            *sum += price;
                        } else {
                            categories.push((event.category, price));
                        }
                    }
                    event_index += 1;
                    if event_index == events.len()
                        || events[event_index].person != person
                        || events[event_index].ts > event.ts.saturating_add(SESSION_GAP)
                    {
                        break;
                    }
                }
                daily.events += (event_index - first_event) as i64;
                daily.sessions += 1;
                daily.revenue += revenue;
                totals[credited].sessions += 1;
                totals[credited].purchasing += i64::from(revenue > 0);
                totals[credited].revenue += revenue;
                if event_index == events.len()
                    || events[event_index].person != person
                    || events[event_index].ts.div_euclid(DAY) as i32 != day
                {
                    break;
                }
            }
            daily.top = categories
                .iter()
                .max_by(|(a, a_sum), (b, b_sum)| a_sum.cmp(b_sum).then_with(|| b.cmp(a)))
                .map(|&(category, _)| category);
            user_rows.push(daily);
        }
    }

    let mut campaign_rows = Vec::new();
    for (day, totals) in campaign_days {
        for (campaign, total) in totals.into_iter().enumerate() {
            if total.sessions > 0 || total.assisted > 0 {
                campaign_rows.push(CampaignDay {
                    campaign: campaign as u32,
                    day,
                    sessions: total.sessions,
                    purchasing: total.purchasing,
                    revenue: total.revenue,
                    assisted: total.assisted,
                });
            }
        }
    }
    (user_rows, campaign_rows)
}
