use crate::{history::History, log::Logger};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_foundation::NSString;
use serde_json::json;

pub(super) struct Source {
    pub pid: u32,
    pub bundle: String,
    pub name: String,
}
pub(super) fn frontmost() -> Option<Source> {
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    let name = app.localizedName()?.to_string();
    let bundle = app
        .bundleIdentifier()
        .map(|value| value.to_string())
        .unwrap_or_else(|| name.clone());
    Some(Source {
        pid: app.processIdentifier() as u32,
        bundle,
        name,
    })
}

#[derive(Clone)]
pub struct Refresher {
    history: History,
    logger: Logger,
}
pub fn start(history: History, logger: Logger) -> Refresher {
    Refresher { history, logger }
}
impl Refresher {
    pub fn refresh(&self, bundle: &str) {
        let outcome = objc2::rc::autoreleasepool(|_| self.refresh_one(bundle));
        if let Err(error) = outcome {
            self.logger
                .event("source_app_refresh", json!({"error":error}));
        }
    }
    fn refresh_one(&self, bundle: &str) -> Result<(), String> {
        let Some(store) = self.history.store() else {
            return Ok(());
        };
        let reader = store.reader().map_err(|error| error.to_string())?;
        let Some(record) = reader
            .source_app_by_path(bundle)
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle),
        );
        let Some(app) = running.firstObject() else {
            return Ok(());
        };
        let name = app
            .localizedName()
            .map(|value| value.to_string())
            .unwrap_or_else(|| bundle.to_owned());
        let mut icon_file = String::new();
        let key: String = super::hash::sha256(bundle.as_bytes())?[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if let Some(data) = app.icon().and_then(|icon| icon.TIFFRepresentation()) {
            let png = super::image::png(&data.to_vec(), 64)?;
            let icons = self.history.icons().ok_or("icons_dir")?;
            std::fs::create_dir_all(&icons).map_err(|error| error.to_string())?;
            icon_file = format!("{key}.png");
            std::fs::write(icons.join(&icon_file), png.png).map_err(|error| error.to_string())?;
        }
        store
            .refresh_source_app(record.id, name, key, icon_file)
            .map_err(|error| error.to_string())
    }
}
