// The user's main agent: the one that leads the group chat, answers Ask on Feed posts (unless the Feed's settings pick
// another) and owns what belongs to no agent, like what the user made on the Scheduled page or what a deleted agent
// left. The profile keeps the pick (`mainAgentId` in the database, app_data.rs, set from an agent's menu on the Agents
// tab); Filo, the agent fresh data starts with (`apps::FILO_AGENT`), until the user picks another. The app makes sure
// it's one of the agents (local_backend.rs `ensure_main_agent`), so the processes reading it here (the MCP server, the
// group's turns) only read.

use crate::app_data;
use crate::apps::FILO_AGENT;
use crate::db::Db;

/// The main agent's id: the one picked while it's an agent, else Filo.
pub fn id(db: &Db) -> String {
    let picked = app_data::profile_text(db, app_data::MAIN_AGENT).ok().flatten();
    picked.filter(|id| app_data::agent(db, id).ok().flatten().is_some()).unwrap_or_else(|| FILO_AGENT.to_string())
}

/// The main agent's name, for what's no agent's; none while it isn't an agent.
pub fn name(db: &Db) -> Option<String> {
    app_data::agent(db, &id(db)).ok().flatten().map(|agent| agent.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_data::tests::agent;

    #[test]
    fn the_pick_counts_while_its_agent_is_there() {
        let db = Db::in_memory().unwrap();
        assert_eq!(id(&db), FILO_AGENT);
        app_data::set_profile(&db, app_data::MAIN_AGENT, Some(&"chef".into())).unwrap();
        assert_eq!(id(&db), FILO_AGENT);
        assert_eq!(name(&db), None);
        app_data::add_agent(&db, &agent("chef", "Chef", None, 1.)).unwrap();
        assert_eq!((id(&db), name(&db).as_deref()), ("chef".to_string(), Some("Chef")));
    }
}
