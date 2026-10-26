// Anabolic (https://github.com/RaptorTwentyOne/Anabolic)
// Copyright (c) 2026 Coşkun Bağrıaçık. SPDX-License-Identifier: MIT OR Apache-2.0
// Reading this to reuse it? You may: keep this notice with what you take,
// and say it comes from Anabolic. That is all the license asks.

//! The ports a programmer leaves open: dev servers, databases, debuggers.
//! Told by the program that listens (a database server names itself) and
//! by the port its tool uses unless told otherwise (Vite's 5173 on
//! node.exe). A port alone names a tool only where nothing else uses it.

/// Servers that name themselves, whatever port they were given.
const SERVERS: &[(&str, &str)] = &[
    ("postgres", "PostgreSQL"),
    ("mysqld", "MySQL"),
    ("mariadbd", "MariaDB"),
    ("mongod", "MongoDB"),
    ("mongos", "MongoDB"),
    ("redis-server", "Redis"),
    ("valkey-server", "Valkey"),
    ("memcached", "Memcached"),
    ("sqlservr", "SQL Server"),
    ("clickhouse", "ClickHouse"),
    ("clickhouse-server", "ClickHouse"),
    ("influxd", "InfluxDB"),
    ("etcd", "etcd"),
    ("consul", "Consul"),
    ("vault", "Vault"),
    ("nats-server", "NATS"),
    ("minio", "MinIO"),
    ("nginx", "nginx"),
    ("httpd", "Apache"),
    ("caddy", "Caddy"),
    ("traefik", "Traefik"),
    ("php-cgi", "PHP"),
    ("php-fpm", "PHP"),
    ("ollama", "Ollama"),
    ("ollama app", "Ollama"),
    ("dockerd", "Docker"),
    ("com.docker.backend", "Docker"),
    ("vpnkit", "Docker"),
    ("wslrelay", "WSL"),
    ("mailpit", "Mailpit"),
    ("mailhog", "MailHog"),
    ("ngrok", "ngrok"),
    ("cloudflared", "Cloudflare Tunnel"),
];

/// Runtimes a dev server runs on: the port then says which. Editors and
/// IDEs are not among them: the ports they open are their own plumbing.
const RUNTIMES: &[(&str, &str)] = &[
    ("node", "Node.js"),
    ("deno", "Deno"),
    ("bun", "Bun"),
    ("python", "Python"),
    ("pythonw", "Python"),
    ("python3", "Python"),
    ("py", "Python"),
    ("uvicorn", "Python"),
    ("gunicorn", "Python"),
    ("java", "Java"),
    ("javaw", "Java"),
    ("dotnet", ".NET"),
    ("iisexpress", "IIS Express"),
    ("ruby", "Ruby"),
    ("php", "PHP"),
    ("erl", "Erlang"),
    ("beam.smp", "Erlang"),
    ("hugo", "Hugo"),
    ("jupyter", "Jupyter"),
    ("jupyter-lab", "Jupyter"),
    ("jupyter-notebook", "Jupyter"),
];

/// What a runtime on this port usually is.
fn on_runtime(runtime: &str, port: u16) -> Option<&'static str> {
    Some(match (runtime, port) {
        ("Node.js" | "Deno" | "Bun", 5173 | 4173 | 24678) => "Vite",
        ("Node.js" | "Deno" | "Bun", 1420) => "Tauri dev server",
        ("Node.js", 4200) => "Angular CLI",
        ("Node.js", 4321) => "Astro",
        ("Node.js", 6006) => "Storybook",
        ("Node.js", 6274 | 6277) => "MCP Inspector",
        ("Node.js", 8081) => "Metro (React Native)",
        ("Node.js", 19000..=19006) => "Expo",
        ("Node.js" | "Deno" | "Bun", 9229 | 9230) => "Node.js debugger",
        ("Node.js", 35729) => "LiveReload",
        ("Python", 8000) => "Django or Python HTTP server",
        ("Python", 5000) => "Flask",
        ("Python", 8888 | 8889) => "Jupyter",
        ("Python", 6006) => "TensorBoard",
        ("Python", 8501) => "Streamlit",
        ("Python", 7860) => "Gradio",
        ("Python", 5678) => "Python debugger (debugpy)",
        ("Java", 5005) => "Java debugger (JDWP)",
        ("Java", 9200 | 9300) => "Elasticsearch",
        ("Java", 9092) => "Kafka",
        ("Java", 2181) => "ZooKeeper",
        ("Java", 8983) => "Solr",
        ("Java", 7474 | 7687) => "Neo4j",
        ("Java", 9042) => "Cassandra",
        ("Erlang", 5672 | 15672) => "RabbitMQ",
        ("Erlang", 5984) => "CouchDB",
        _ => return None,
    })
}

/// Ports nothing but their tool uses.
fn by_port(port: u16) -> Option<&'static str> {
    Some(match port {
        5432 => "PostgreSQL",
        3306 => "MySQL",
        6379 => "Redis",
        27017 => "MongoDB",
        11211 => "Memcached",
        1433 => "SQL Server",
        9200 => "Elasticsearch",
        5672 | 15672 => "RabbitMQ",
        9092 => "Kafka",
        5984 => "CouchDB",
        7474 => "Neo4j",
        8086 => "InfluxDB",
        26257 => "CockroachDB",
        9042 => "Cassandra",
        2375 | 2376 => "Docker API",
        6443 => "Kubernetes API",
        8200 => "Vault",
        8500 => "Consul",
        4566 => "LocalStack",
        5601 => "Kibana",
        16686 => "Jaeger",
        4317 | 4318 => "OpenTelemetry collector",
        11434 => "Ollama",
        5173 => "Vite",
        9229 => "Node.js debugger",
        5005 => "Java debugger (JDWP)",
        9222 => "Chrome remote debugging",
        _ => return None,
    })
}

/// The program's name as the tables list it, without ".exe" and case.
fn stem(program: &str) -> String {
    let lower = program.to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_string(),
        None => lower,
    }
}

/// The development tool behind a listening port, if it looks like one.
pub fn tool(port: u16, program: &str) -> Option<&'static str> {
    let stem = stem(program);
    if let Some((_, server)) = SERVERS.iter().find(|(s, _)| *s == stem) {
        return Some(server);
    }
    // Versioned interpreters: python3.12, node22.
    let runtime = RUNTIMES.iter().find(|(r, _)| {
        stem == *r
            || stem
                .strip_prefix(r)
                .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
    });
    match runtime {
        Some((_, runtime)) => on_runtime(runtime, port).or(Some(runtime)),
        None => by_port(port),
    }
}

#[cfg(test)]
mod tests {
    use super::tool;

    #[test]
    fn names_the_tool_by_program_and_port() {
        assert_eq!(tool(5173, "node.exe"), Some("Vite"));
        assert_eq!(tool(3000, "node.exe"), Some("Node.js"));
        assert_eq!(tool(9229, "node"), Some("Node.js debugger"));
        assert_eq!(
            tool(8000, "python3.12"),
            Some("Django or Python HTTP server")
        );
        assert_eq!(tool(5005, "java.exe"), Some("Java debugger (JDWP)"));
        // A server names itself on any port.
        assert_eq!(tool(15432, "postgres.exe"), Some("PostgreSQL"));
        assert_eq!(tool(6380, "redis-server"), Some("Redis"));
        // A port that only its tool uses, held by something unknown.
        assert_eq!(tool(5432, "something.exe"), Some("PostgreSQL"));
    }

    #[test]
    fn leaves_ordinary_programs_alone() {
        assert_eq!(tool(3000, "grafana.exe"), None);
        assert_eq!(tool(443, "svchost.exe"), None);
        assert_eq!(tool(57621, "Spotify.exe"), None);
        assert_eq!(
            tool(8080, "nodejs-helper.exe"),
            None,
            "not a runtime's versioned name"
        );
    }
}
