use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::reload::Handle as ReloadHandleInner;
use tracing_subscriber::reload::Layer as ReloadLayer;
use tracing_subscriber::Registry;

/// Type alias for the reload handle.
pub type ReloadHandle = ReloadHandleInner<EnvFilter, Registry>;

/// Type alias for the reload layer.
pub type ReloadFilterLayer = ReloadLayer<EnvFilter, Registry>;
