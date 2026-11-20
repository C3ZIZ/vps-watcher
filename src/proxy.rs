//! Ready-to-use reverse proxy configs, so going public is copy and paste.

use std::net::SocketAddr;

use crate::config::Config;

/// Address the proxy should forward to (0.0.0.0 becomes 127.0.0.1).
fn upstream(listen: SocketAddr) -> String {
    let ip = listen.ip();
    if ip.is_unspecified() {
        if ip.is_ipv4() { format!("127.0.0.1:{}", listen.port()) } else { format!("[::1]:{}", listen.port()) }
    } else {
        listen.to_string()
    }
}

/// Caddyfile site block; Caddy gets and renews the HTTPS certificate itself.
pub fn caddy(cfg: &Config) -> Result<String, String> {
    let domain = cfg.domain.as_deref().ok_or(NO_DOMAIN)?;
    Ok(format!(
        "# vps-watcher: paste into /etc/caddy/Caddyfile, then run: sudo systemctl reload caddy\n\
         {domain} {{\n\
         \tencode zstd gzip\n\
         \treverse_proxy {up} {{\n\
         \t\t# Send live updates immediately.\n\
         \t\tflush_interval -1\n\
         \t}}\n\
         }}\n",
        up = upstream(cfg.listen)
    ))
}

/// nginx server block plus the certbot command that adds HTTPS.
pub fn nginx(cfg: &Config) -> Result<String, String> {
    let domain = cfg.domain.as_deref().ok_or(NO_DOMAIN)?;
    Ok(format!(
        "# vps-watcher: save as /etc/nginx/sites-available/vps-watcher, then run:\n\
         #   sudo ln -s /etc/nginx/sites-available/vps-watcher /etc/nginx/sites-enabled/\n\
         #   sudo nginx -t && sudo systemctl reload nginx\n\
         #   sudo certbot --nginx -d {domain}\n\
         server {{\n\
         \x20   listen 80;\n\
         \x20   listen [::]:80;\n\
         \x20   server_name {domain};\n\
         \n\
         \x20   location / {{\n\
         \x20       proxy_pass http://{up};\n\
         \x20       proxy_http_version 1.1;\n\
         \x20       proxy_set_header Host $host;\n\
         \x20       proxy_set_header X-Real-IP $remote_addr;\n\
         \x20       proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n\
         \x20       proxy_set_header X-Forwarded-Proto $scheme;\n\
         \x20       # Live updates use Server-Sent Events: no buffering, long reads.\n\
         \x20       proxy_buffering off;\n\
         \x20       proxy_read_timeout 1h;\n\
         \x20   }}\n\
         }}\n",
        up = upstream(cfg.listen)
    ))
}

const NO_DOMAIN: &str = "set VPSW_DOMAIN (for example in /etc/vps-watcher/vps-watcher.env) first";

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(domain: Option<&str>, listen: &str) -> Config {
        let mut c = Config::from_lookup(|_| None).unwrap();
        c.domain = domain.map(str::to_string);
        c.listen = listen.parse().unwrap();
        c
    }

    #[test]
    fn needs_domain() {
        assert!(caddy(&cfg(None, "127.0.0.1:9100")).is_err());
        assert!(nginx(&cfg(None, "127.0.0.1:9100")).is_err());
    }

    #[test]
    fn renders_domain_and_upstream() {
        let c = caddy(&cfg(Some("watch.example.com"), "0.0.0.0:9200")).unwrap();
        assert!(c.contains("watch.example.com {"));
        assert!(c.contains("reverse_proxy 127.0.0.1:9200"));
        let n = nginx(&cfg(Some("watch.example.com"), "127.0.0.1:9100")).unwrap();
        assert!(n.contains("server_name watch.example.com;"));
        assert!(n.contains("proxy_pass http://127.0.0.1:9100;"));
        assert!(n.contains("proxy_buffering off;"));
    }
}
