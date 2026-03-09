use crate::logistics_planner::{
    Action, LogisticShip, PlannerConstraints, ScheduledAction, ShipSchedule, Task, TaskActions,
};
use crate::models::WaypointSymbol;
use std::collections::BTreeMap;
use std::sync::Arc;

use super::value_feature::JobValueDimension as _;
use vrp_core::models::common::*;
use vrp_core::models::problem::*;
use vrp_core::prelude::*;
use vrp_core::rosomaxa::prelude::TelemetryMode;

struct Planner<'a> {
    ships: &'a [LogisticShip],
    tasks: &'a [Task],
    market_waypoints: &'a [WaypointSymbol],
    duration_matrix: &'a [Vec<f64>],
    distance_matrix: &'a [Vec<f64>],
    constraints: &'a PlannerConstraints,
}

#[derive(Debug)]
struct Activity {
    waypoint: WaypointSymbol,
    action: Action,
    task_id: String,
    completes_task: bool,
}

impl<'a> Planner<'a> {
    fn waypoint_index(&self, waypoint: &WaypointSymbol) -> usize {
        self.market_waypoints
            .iter()
            .position(|w| w == waypoint)
            .unwrap()
    }

    pub fn translate_problem(&self) -> (Arc<Problem>, BTreeMap<String, Activity>) {
        let mut job_id_map: BTreeMap<String, Activity> = BTreeMap::new();

        let max_duration = self.constraints.plan_length as f64;
        let jobs: Vec<Job> = self
            .tasks
            .iter()
            .map(|task| match &task.actions {
                TaskActions::VisitLocation { waypoint, action } => {
                    let job_id = format!("Visit-{}-{:?}", waypoint, action);
                    job_id_map.insert(
                        job_id.clone(),
                        Activity {
                            waypoint: waypoint.clone(),
                            action: action.clone(),
                            task_id: task.id.clone(),
                            completes_task: true,
                        },
                    );
                    
                    SingleBuilder::default()
                        .id(&job_id)
                        .location(self.waypoint_index(waypoint))
                        .unwrap()
                        .times(vec![TimeWindow::new(0.0, max_duration)])
                        .unwrap()
                        .dimension(|dimens| {
                            dimens.set_job_value(task.value as f64);
                        })
                        .build_as_job()
                        .unwrap()
                }
                TaskActions::TransportCargo {
                    src,
                    dest,
                    src_action,
                    dest_action,
                } => {
                    let (good, units) = match src_action {
                        Action::BuyGoods(good, units) => (good, units),
                        _ => panic!("unexpected source action"),
                    };
                    let (dest_good, dest_units) = match dest_action {
                        Action::SellGoods(good, units) => (good, units),
                        Action::DeliverConstruction(good, units) => (good, units),
                        Action::DeliverContract(good, units) => (good, units),
                        _ => panic!("unexpected destination action"),
                    };
                    assert_eq!(good, dest_good);
                    assert_eq!(units, dest_units);
                    let job_id = format!("Transport-{}", good);
                    let buy_job_id = format!("buy/{}/{}", units, good);
                    let sell_job_id = format!("sell/{}/{}", units, good);
                    let job = MultiBuilder::default()
                        .id(&job_id)
                        .add_job(
                            SingleBuilder::default()
                                .id(&buy_job_id)
                                .demand(Demand::pudo_pickup(*units as i32))
                                .location(self.waypoint_index(src))
                                .unwrap()
                                .times(vec![TimeWindow::new(0.0, max_duration)])
                                .unwrap()
                                .build()
                                .unwrap(),
                        )
                        .add_job(
                            SingleBuilder::default()
                                .id(&sell_job_id)
                                .demand(Demand::pudo_delivery(*units as i32))
                                .location(self.waypoint_index(dest))
                                .unwrap()
                                .times(vec![TimeWindow::new(0.0, max_duration)])
                                .unwrap()
                                .build()
                                .unwrap(),
                        )
                        .dimension(|dimens| {
                            dimens.set_job_value(task.value as f64);
                        })
                        .build_as_job()
                        .unwrap();
                    job_id_map.insert(
                        buy_job_id,
                        Activity {
                            waypoint: src.clone(),
                            action: src_action.clone(),
                            task_id: task.id.clone(),
                            completes_task: false,
                        },
                    );
                    job_id_map.insert(
                        sell_job_id,
                        Activity {
                            waypoint: dest.clone(),
                            action: dest_action.clone(),
                            task_id: task.id.clone(),
                            completes_task: true,
                        },
                    );
                    job
                }
            })
            .collect();

        let duration_matrix = self
            .duration_matrix
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let distance_matrix = self
            .distance_matrix
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        let routing_matrix =
            Arc::new(SimpleTransportCost::new(duration_matrix, distance_matrix).unwrap());

        let vehicles: Vec<Vehicle> = self
            .ships
            .iter()
            .map(|ship| {
                VehicleBuilder::default()
                    .id(&ship.symbol)
                    .add_detail(
                        VehicleDetailBuilder::default()
                            .set_start_location(self.waypoint_index(&ship.start_waypoint))
                            .set_start_time(0.0)
                            .build()
                            .unwrap(),
                    )
                    .capacity(SingleDimLoad::new(ship.capacity as i32))
                    .build()
                    .unwrap()
            })
            .collect();

        // Goal
        let capacity_feature = CapacityFeatureBuilder::<SingleDimLoad>::new("capacity")
            .build()
            .unwrap();
        let transport_feature = TransportFeatureBuilder::new("min-distance")
            .set_transport_cost(routing_matrix.clone())
            .set_time_constrained(true)
            .build_minimize_duration()
            .unwrap();
        let minimize_unassigned = MinimizeUnassignedBuilder::new("min-unassigned")
            .build()
            .unwrap();
        let max_value_feature = super::value_feature::feature_layer();
        let goal = GoalContextBuilder::with_features(&[
            max_value_feature,
            minimize_unassigned,
            transport_feature,
            capacity_feature,
        ])
        .unwrap()
        .build()
        .unwrap();

        let problem = ProblemBuilder::default()
            .add_jobs(jobs.into_iter())
            .add_vehicles(vehicles.into_iter())
            .with_goal(goal)
            .with_transport_cost(routing_matrix.clone())
            .build()
            .unwrap();

        (Arc::new(problem), job_id_map)
    }
}

pub fn run_planner(
    ships: &[LogisticShip],
    tasks: &[Task],
    market_waypoints: &[WaypointSymbol],
    duration_matrix: &[Vec<f64>],
    distance_matrix: &[Vec<f64>],
    constraints: &PlannerConstraints,
) -> Vec<ShipSchedule> {
    let planner = Planner {
        ships,
        tasks,
        market_waypoints,
        duration_matrix,
        distance_matrix,
        constraints,
    };
    let (problem, job_id_map) = planner.translate_problem();

    let config = VrpConfigBuilder::new(problem.clone())
        .set_telemetry_mode(TelemetryMode::None)
        .prebuild()
        .unwrap()
        .with_max_time(Some(constraints.max_compute_time.num_seconds() as usize))
        .with_max_generations(Some(3000))
        .build()
        .unwrap();

    let solution = Solver::new(problem.clone(), config).solve().unwrap();

    
    ships
        .iter()
        .map(|ship| {
            let route = solution
                .routes
                .iter()
                .find(|route| route.actor.vehicle.dimens.get_vehicle_id().unwrap() == &ship.symbol);
            let route = match route {
                Some(route) => route,
                None => {
                    return ShipSchedule {
                        ship: ship.clone(),
                        actions: vec![],
                    };
                }
            };
            let mut actions = vec![];
            for activity in route.tour.all_activities() {
                let arrival = activity.schedule.arrival;
                assert_eq!(activity.place.idx, 0); // Our tasks can only be performed in 1 place
                let waypoint_symbol = &market_waypoints[activity.place.location];
                if let Some(job) = &activity.job {
                    let job_id = job.dimens.get_job_id().unwrap();
                    let activity = &job_id_map[job_id.as_str()];
                    assert_eq!(&activity.waypoint, waypoint_symbol);

                    actions.push(ScheduledAction {
                        waypoint: activity.waypoint.clone(),
                        action: activity.action.clone(),
                        timestamp: arrival,
                        task_id: activity.task_id.clone(),
                        completes_task: activity.completes_task,
                    });
                }
            }
            ShipSchedule {
                ship: ship.clone(),
                actions,
            }
        })
        .collect()
}

#[cfg(test)]
mod test {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_run_planner() {
        pretty_env_logger::formatted_timed_builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Debug)
            .try_init()
            .ok();
        let ships = vec![
            LogisticShip {
                symbol: "SHIP1".to_string(),
                capacity: 100,
                speed: 10,
                start_waypoint: WaypointSymbol::new("X1-S1-W1"),
            },
            LogisticShip {
                symbol: "SHIP2".to_string(),
                capacity: 200,
                speed: 20,
                start_waypoint: WaypointSymbol::new("X1-S1-W1"),
            },
        ];
        let tasks = vec![
            Task {
                id: "TASK1".to_string(),
                actions: TaskActions::VisitLocation {
                    waypoint: WaypointSymbol::new("X1-S1-W1"),
                    action: Action::RefreshMarket,
                },
                value: 1000,
            },
            Task {
                id: "TASK2".to_string(),
                actions: TaskActions::VisitLocation {
                    waypoint: WaypointSymbol::new("X1-S1-W2"),
                    action: Action::RefreshShipyard,
                },
                value: 1000,
            },
            Task {
                id: "TASK3".to_string(),
                actions: TaskActions::TransportCargo {
                    src: WaypointSymbol::new("X1-S1-W1"),
                    dest: WaypointSymbol::new("X1-S1-W2"),
                    src_action: Action::BuyGoods("FOOD".to_string(), 10),
                    dest_action: Action::SellGoods("FOOD".to_string(), 10),
                },
                value: 5000,
            },
        ];
        let constraints = PlannerConstraints {
            plan_length: 24 * 60 * 60,
            max_compute_time: Duration::seconds(1),
        };
        let market_waypoints = vec![
            WaypointSymbol::new("X1-S1-W1"),
            WaypointSymbol::new("X1-S1-W2"),
        ];
        let duration_matrix = vec![vec![0.0, 100.0], vec![100.0, 0.0]];
        let distance_matrix = vec![vec![0.0, 100.0], vec![100.0, 0.0]];
        let schedule = run_planner(
            &ships,
            &tasks,
            &market_waypoints,
            &duration_matrix,
            &distance_matrix,
            &constraints,
        );
        println!("schedule: {:?}", schedule);
        assert_eq!(schedule.len(), 2);
    }
}
