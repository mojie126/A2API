use std::sync::{atomic::Ordering, Arc};

use agent2api_provider_protocol::WireError;

use crate::server::core::plugin_host::wasm::catpaw_output_unconfirmed as unconfirmed;

impl super::RequestTelemetry {
    pub(in crate::server) fn install_catpaw_output(
        &self,
        output: Arc<crate::server::core::plugin_host::wasm::CatPawOutputSlot>,
    ) -> Result<(), WireError> {
        if self.catpaw_output_taken.load(Ordering::Acquire) { return Err(unconfirmed()); }
        let mut deferred = None;
        let result = (|| {
            let mut slot = self.catpaw_output.try_lock().map_err(|_| unconfirmed())?;
            if self.catpaw_output_taken.load(Ordering::Acquire) { return Err(unconfirmed()); }
            if slot.as_ref().is_some_and(|old| !old.replaceable()) { return Err(unconfirmed()); }
            deferred = slot.replace(output);
            Ok(())
        })();
        drop(deferred);
        result
    }

    pub(in crate::server) fn take_catpaw_output(
        &self,
    ) -> Result<Option<crate::server::core::plugin_host::wasm::CatPawOutputGuard>, WireError> {
        if self.catpaw_output_taken.load(Ordering::Acquire) {
            return Err(unconfirmed());
        }
        let slot = {
            let mut slot = self.catpaw_output.try_lock().map_err(|_| unconfirmed())?;
            if self.catpaw_output_taken.load(Ordering::Acquire) { return Err(unconfirmed()); }
            if slot.is_none() { return Ok(None); }
            if self.catpaw_output_taken.swap(true, Ordering::AcqRel) {
                return Err(unconfirmed());
            }
            slot.take()
        };
        match slot {
            Some(slot) => slot.take(),
            None => Ok(None),
        }
    }
}
