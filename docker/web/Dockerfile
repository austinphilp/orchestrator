FROM nginx:1.27-alpine

COPY docker/web/nginx.conf /etc/nginx/conf.d/default.conf
COPY crates/orchestrator-web/static/ /usr/share/nginx/html/

