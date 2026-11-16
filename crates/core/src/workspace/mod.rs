//! What the application derives from workspace state: where threads stand, how they are ordered,
//! and who coordinates whom. The reducer and the view projection both read these.

pub mod activity;
pub mod coordinated_status;
pub mod coordination;
pub mod coordination_messages;
pub mod handles;
pub mod location;
pub mod member_facts;
pub mod notices;
pub mod order;
pub mod outstanding_stops;
pub mod persistence;
pub mod preferences;
pub mod thread_starts;
