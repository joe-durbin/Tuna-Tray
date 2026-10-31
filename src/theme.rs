//! Light/dark detection from the desktop. KDE publishes its colour scheme through
//! the freedesktop appearance portal (xdg-desktop-portal-kde) as
//! org.freedesktop.appearance / color-scheme: 1 = prefer dark, 2 = prefer light,
//! 0 = no preference. The portal emits SettingChanged when the user switches schemes.

use std::thread;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.Settings";
const NAMESPACE: &str = "org.freedesktop.appearance";
const KEY: &str = "color-scheme";

/// Reads the current preference. Dark unless the desktop explicitly asks for light.
pub fn prefers_dark(conn: &Connection) -> bool {
    let value = Proxy::new(conn, DEST, PATH, IFACE)
        .and_then(|proxy| proxy.call::<_, _, OwnedValue>("ReadOne", &(NAMESPACE, KEY)));
    match value {
        Ok(value) => scheme_is_dark(value),
        Err(_) => true,
    }
}

/// Calls `on_change` with the new preference whenever the desktop changes its colour scheme.
pub fn watch(on_change: impl Fn(bool) + Send + 'static) {
    thread::spawn(move || {
        let Ok(conn) = Connection::session() else { return };
        let Ok(proxy) = Proxy::new(&conn, DEST, PATH, IFACE) else { return };
        let Ok(signals) = proxy.receive_signal("SettingChanged") else { return };
        for message in signals {
            let Ok((namespace, key, value)) = message.body().deserialize::<(String, String, OwnedValue)>() else {
                continue;
            };
            if namespace == NAMESPACE && key == KEY {
                on_change(scheme_is_dark(value));
            }
        }
    });
}

fn scheme_is_dark(value: OwnedValue) -> bool {
    let value: Value = value.into();
    let scheme = match value {
        Value::Value(inner) => u32::try_from(*inner).ok(),
        other => u32::try_from(other).ok(),
    };
    scheme != Some(2)
}
