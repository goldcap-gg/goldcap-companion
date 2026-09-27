//! WoW: Forever. The player's own auction house scan (`GoldCapDB.foreverScan.fold`) goes up
//! to goldcap.gg, and every player's prices come back into the Forever install's
//! `GoldCap_AppData`. The wire is the site's (plan 2a Tasks 3 and 9; docs/companion/AGENTS.md
//! "WoW: Forever upload contract").

use mlua::{Lua, LuaOptions, StdLib, Value};

/// WoW: Forever's interface numbers: 16001 in the beta. The same range as the addon's
/// `GC.Game.FOREVER_MIN_INTERFACE`/`FOREVER_MAX_INTERFACE` and the API's `isForeverInterface`.
// used from Task C5 on
#[allow(dead_code)]
pub const FOREVER_MIN_INTERFACE: i64 = 16_000;
// used from Task C5 on
#[allow(dead_code)]
pub const FOREVER_MAX_INTERFACE: i64 = 16_999;

// used from Task C5 on
#[allow(dead_code)]
pub fn is_forever_interface(interface: i64) -> bool {
    (FOREVER_MIN_INTERFACE..=FOREVER_MAX_INTERFACE).contains(&interface)
}

/// `GoldCapDB.client`, as far as discovery needs it.
// used from Task C5 on
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq)]
pub struct Passport {
    pub interface: i64,
    pub build: String,
    pub region_id: Option<i64>,
}

// used from Task C5 on
#[allow(dead_code)]
fn globals_db(lua: &Lua, source: &str) -> Option<mlua::Table> {
    lua.load(source).exec().ok()?;
    match lua.globals().get::<Value>("GoldCapDB") {
        Ok(Value::Table(db)) => Some(db),
        _ => None,
    }
}

/// The passport a SavedVariables file carries, or None. Anything unreadable is "no passport".
// used from Task C5 on
#[allow(dead_code)]
pub fn read_passport(source: &str) -> Option<Passport> {
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default()).ok()?;
    let db = globals_db(&lua, source)?;
    let Ok(Value::Table(client)) = db.get::<Value>("client") else { return None };
    let interface = crate::savedvars::opt_int(&client, "interface")?;
    let build = crate::savedvars::opt_string(&client, "build")?.trim().to_string();
    if !crate::savedvars::is_valid_passport_build(&build) {
        return None;
    }
    Some(Passport { interface, build, region_id: crate::savedvars::opt_int(&client, "regionId") })
}
