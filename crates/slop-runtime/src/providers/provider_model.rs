/// One model offered by a provider and the wire shape it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderModel {
    /// Opaque model id sent to the provider, e.g. `glm-5.3-flash`.
    pub id: &'static str,
    /// Human-readable label from the provider docs.
    pub display_name: &'static str,
    /// Which HTTP API shape serves this model.
    pub wire: WireProtocol,
}
use super::WireProtocol;
