use crate::domain::VenueId;
use crate::venues::{VenueRuntime, VenueWatcher};
use tokio::task::JoinHandle;
use tracing::warn;

pub struct StubVenueWatcher {
    venue: VenueId,
    program_id: Option<String>,
}

impl StubVenueWatcher {
    pub fn new(venue: VenueId, program_id: Option<String>) -> Self {
        Self { venue, program_id }
    }
}

impl VenueWatcher for StubVenueWatcher {
    fn name(&self) -> &'static str {
        "stub_watcher"
    }

    fn spawn(self: Box<Self>, _runtime: VenueRuntime) -> JoinHandle<()> {
        tokio::spawn(async move {
            warn!(
                venue = %self.venue.slug(),
                display = %self.venue.display_name,
                program_id = ?self.program_id,
                "venue is enabled but watcher is not implemented yet"
            );
        })
    }
}
