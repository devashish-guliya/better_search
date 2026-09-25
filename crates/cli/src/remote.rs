//! `bs query`: searches through the background service instead of a local index.

use std::io;
use std::time::{Duration, Instant};

use bs_engine::fmt::{count as fmt_count, duration as fmt_duration};
use bs_pipe::{Client, Status};

use crate::BENCH_QUERIES;

pub const USAGE: &str = "\
Usage:
  bs query [-n COUNT] TEXT...   Search through the running better_search service.
                                No admin needed. Other users' profile folders are hidden.
  bs query --bench              Time searches through the pipe: round trip, time spent
                                searching in the service, and the difference (overhead).";

pub fn run(args: &[String]) -> Result<(), String> {
    let mut limit: u16 = 20;
    let mut bench = false;
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--bench" => bench = true,
            "-n" => {
                let value = it.next().ok_or("-n needs a number")?;
                limit = value
                    .parse()
                    .map_err(|_| format!("invalid count: {value}"))?;
            }
            word => words.push(word),
        }
    }
    if bench {
        return run_bench();
    }
    if words.is_empty() {
        return Err(format!("nothing to search for\n\n{USAGE}"));
    }
    let text = words.join(" ");

    let t = Instant::now();
    let mut client = connect()?;
    let connect_time = t.elapsed();
    let t = Instant::now();
    let reply = client.search(&text, limit).map_err(|e| e.to_string())?;
    let round_trip = t.elapsed();
    check(reply.status)?;
    for (i, hit) in reply.hits.iter().enumerate() {
        let slash = if hit.is_dir { "\\" } else { "" };
        println!("{:>3}. [{:>4}] {}{slash}", i + 1, hit.score, hit.path);
    }
    println!(
        "     {} matches · round trip {} (service {}) · connect {}",
        fmt_count(reply.total_matches as usize),
        fmt_duration(round_trip),
        fmt_duration(Duration::from_micros(u64::from(reply.search_micros))),
        fmt_duration(connect_time)
    );
    Ok(())
}

fn connect() -> Result<Client, String> {
    Client::connect().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => "the better_search service is not running".to_string(),
        _ => format!("cannot connect to the better_search service: {e}"),
    })
}

fn check(status: Status) -> Result<(), String> {
    match status {
        Status::Ok => Ok(()),
        Status::Loading => Err("the service is still loading the index; try again soon".into()),
        Status::BadRequest => Err("the service rejected the request".into()),
        Status::Denied => Err("the service could not identify this user".into()),
    }
}

fn run_bench() -> Result<(), String> {
    const RUNS: usize = 15;
    let mut connects: Vec<Duration> = (0..RUNS)
        .map(|_| {
            let t = Instant::now();
            let client = connect();
            let elapsed = t.elapsed();
            drop(client);
            elapsed
        })
        .collect();
    connects.sort();
    println!("Connect: median {}", fmt_duration(connects[RUNS / 2]));

    let mut client = connect()?;
    // The first reply may have to build the privacy map or page the index back in.
    let t = Instant::now();
    check(
        client
            .search("warm up", 20)
            .map_err(|e| e.to_string())?
            .status,
    )?;
    println!(
        "First search on the connection: {}",
        fmt_duration(t.elapsed())
    );
    println!();
    println!("Same query repeated on one connection, 20 results (median of {RUNS}):");
    println!(
        "{:<14} {:>12} {:>12} {:>10} {:>10}",
        "query", "matches", "round trip", "service", "overhead"
    );
    for &text in BENCH_QUERIES {
        let mut round = Vec::with_capacity(RUNS);
        let mut service = Vec::with_capacity(RUNS);
        let mut overhead = Vec::with_capacity(RUNS);
        let mut matches = 0;
        for _ in 0..RUNS {
            let t = Instant::now();
            let reply = client.search(text, 20).map_err(|e| e.to_string())?;
            let elapsed = t.elapsed();
            check(reply.status)?;
            let svc = Duration::from_micros(u64::from(reply.search_micros));
            matches = reply.total_matches;
            round.push(elapsed);
            service.push(svc);
            overhead.push(elapsed.saturating_sub(svc));
        }
        for v in [&mut round, &mut service, &mut overhead] {
            v.sort();
        }
        println!(
            "{:<14} {:>12} {:>12} {:>10} {:>10}",
            format!("\"{text}\""),
            fmt_count(matches as usize),
            fmt_duration(round[RUNS / 2]),
            fmt_duration(service[RUNS / 2]),
            fmt_duration(overhead[RUNS / 2])
        );
    }

    let t = Instant::now();
    let reply = client.search("e", 1000).map_err(|e| e.to_string())?;
    let elapsed = t.elapsed();
    check(reply.status)?;
    let svc = Duration::from_micros(u64::from(reply.search_micros));
    println!();
    println!(
        "1000 results for \"e\": round trip {} (service {}, overhead {})",
        fmt_duration(elapsed),
        fmt_duration(svc),
        fmt_duration(elapsed.saturating_sub(svc))
    );
    Ok(())
}
