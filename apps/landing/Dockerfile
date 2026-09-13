FROM caddy:2-alpine

ENV PORT=8080 \
    XDG_CONFIG_HOME=/tmp/caddy/config \
    XDG_DATA_HOME=/tmp/caddy/data

COPY Caddyfile /etc/caddy/Caddyfile
COPY dist/ /srv/

USER 10001:10001
EXPOSE 8080
