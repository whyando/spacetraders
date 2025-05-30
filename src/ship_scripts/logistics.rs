use std::{collections::BTreeMap, sync::Arc};

use crate::{
    database::DbClient, models::LogisticsScriptConfig, ship_controller::ShipController,
    tasks::LogisticTaskManager,
};
use chrono::Duration;
use log::*;

pub async fn run(
    ship_controller: ShipController,
    db: DbClient,
    taskmanager: Arc<LogisticTaskManager>,
    config: LogisticsScriptConfig,
) {
    info!("Starting script logistics for {}", ship_controller.symbol());
    ship_controller.wait_for_transit().await;

    let ship_symbol = ship_controller.symbol();
    let system_symbol = ship_controller.system();

    loop {
        // Generate or resume schedule
        // !! it would be better if script was not implementing persistence, and instead relied on the task manager for it's persistent state
        let schedule_opt = db.load_schedule(&ship_symbol).await;
        let progress_opt = db.load_schedule_progress(&ship_symbol).await;
        assert_eq!(schedule_opt.is_some(), progress_opt.is_some());
        let resume_saved = match (&schedule_opt, progress_opt) {
            (Some(schedule), Some(progress)) => progress < schedule.actions.len(),
            _ => false,
        };

        let (schedule, progress) = if resume_saved {
            (schedule_opt.unwrap(), progress_opt.unwrap())
        } else {
            // sell fuel if we have fuel in cargo, after warps
            let fuel_units = ship_controller.cargo_good_count("FUEL");
            if fuel_units > 0 {
                ship_controller.sell_goods("FUEL", fuel_units, false).await;
            }
            assert!(ship_controller.cargo_empty());

            // Generate new schedule
            let plan_length = Duration::try_minutes(15).unwrap();
            let schedule = taskmanager
                .take_tasks(
                    &ship_symbol,
                    &system_symbol,
                    &config,
                    ship_controller.cargo_capacity(),
                    ship_controller.engine_speed(),
                    ship_controller.fuel_capacity(),
                    &ship_controller.waypoint(),
                    plan_length,
                )
                .await;
            db.save_schedule(&ship_symbol, &schedule).await;
            db.save_schedule_progress(&ship_symbol, 0).await;
            (schedule, 0)
        };

        let schedule_len = schedule.actions.len();
        if schedule_len == 0 {
            info!(
                "Ship {} was scheduled no tasks to perform. Sleeping 5-10 minutes.",
                ship_controller.symbol()
            );
            let rand_seconds = rand::random::<u64>() % 300;
            tokio::time::sleep(tokio::time::Duration::from_secs(300 + rand_seconds)).await;
            continue;
        }

        // sanity check before we start (up to index 'progress')
        let mut expected_cargo = BTreeMap::new();
        for action in schedule.actions.iter().take(progress) {
            let net_cargo = action.action.net_cargo();
            if let Some((good, amount)) = net_cargo {
                *expected_cargo.entry(good).or_insert(0) += amount;
            }
        }
        expected_cargo.retain(|_, &mut v| v != 0);
        let cargo_correct = expected_cargo == ship_controller.cargo_map();

        let cargo_without_fuel = {
            let mut m = ship_controller.cargo_map();
            m.remove("FUEL");
            m
        };
        let cargo_correct_except_fuel = expected_cargo == cargo_without_fuel;

        let next_action = schedule.actions.get(progress).unwrap();
        if let Some((good, amount)) = next_action.action.net_cargo() {
            *expected_cargo.entry(good).or_insert(0) += amount;
        }
        expected_cargo.retain(|_, &mut v| v != 0);
        let cargo_correct1 = expected_cargo == ship_controller.cargo_map();

        let mut actions_to_skip = 0usize;
        if !cargo_correct {
            warn!(
                "Ship {} cargo is incorrect. Expected: {:?}, Actual: {:?}",
                ship_controller.symbol(),
                expected_cargo,
                ship_controller.cargo_map()
            );
            if cargo_correct1 {
                info!(
                    "Ship {} cargo would be correct after performing 1 action {:?}. Skipping action.",
                    ship_controller.symbol(),
                    next_action
                );
                actions_to_skip = 1;
            } else if cargo_correct_except_fuel {
                info!(
                    "Ship {} cargo would be correct after dropping excess fuel.",
                    ship_controller.symbol(),
                );
                let units = ship_controller.cargo_good_count("FUEL");
                ship_controller.sell_goods("FUEL", units, false).await;
            } else {
                // ship_controller.sell_goods("FABRICS", 4).await; // manual fix
                panic!("Couldn't recover cargo state");
            }
        }

        // execute
        for (action_idx, scheduled_action) in schedule.actions.iter().enumerate().skip(progress) {
            ship_controller
                .goto_waypoint(&scheduled_action.waypoint)
                .await;
            // perform action
            if actions_to_skip == 0 {
                ship_controller
                    .execute_action(&scheduled_action.action)
                    .await;
            } else {
                actions_to_skip -= 1;
            }

            // log action completion, so we can resume from this point if we crash
            db.update_schedule_progress(&ship_symbol, action_idx + 1)
                .await;
            if let Some(task_id) = &scheduled_action.completes_task_id {
                taskmanager.set_task_completed(task_id).await;
            }
        }
        info!(
            "Ship {} completed {} tasks",
            ship_controller.symbol(),
            schedule_len
        );
    }

    // info!("Finished script logistics for {}", ship_controller.symbol());
}
