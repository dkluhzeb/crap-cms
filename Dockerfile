FROM alpine:3.20
RUN apk add --no-cache ca-certificates
ARG BINARY=crap-cms-linux-x86_64
COPY ${BINARY} /usr/local/bin/crap-cms
RUN chmod +x /usr/local/bin/crap-cms
COPY example/ /example/
RUN crap-cms -C /example migrate up
VOLUME ["/config"]
EXPOSE 3000 50051
ENTRYPOINT ["crap-cms"]
CMD ["-C", "/config", "serve"]
