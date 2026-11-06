#!/usr/bin/env bash
# Runs as root inside Bazza's dedicated Ubuntu WSL2 distribution.
set -Eeuo pipefail
umask 077

mode=${1:?}
windows_host=${2:?}
api_port=${3:?}
callback_port=${4:?}
[[ $mode == Setup || $mode == Start ]]
[[ $windows_host =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]
[[ $api_port =~ ^[0-9]{4,5}$ && $callback_port =~ ^[0-9]{4,5}$ ]]

revision=eaacec16f2939b595f196b3957a717bbb0ed23e9
archive_sha=cb257da4ec91162c2c82a2d36b7d6b1d9037133e8c98fb1228b86b2fde242e37
image=bazza-attendee-local:$revision
home_dir=/opt/bazza-attendee
install -d -m 700 "$home_dir"
cd "$home_dir"
exec 9>setup.lock
flock -n 9

if [[ $mode == Start ]]; then
    test -f initialized
elif ! command -v docker >/dev/null || ! docker compose version >/dev/null 2>&1 \
    || ! docker buildx version >/dev/null 2>&1 || ! command -v curl >/dev/null; then
    # Configure only a fresh engine, before the package can start its daemon.
    # A storage driver change would hide an existing engine's images and containers.
    if ! command -v dockerd >/dev/null && [[ ! -e /etc/docker/daemon.json \
        && ! -L /etc/docker/daemon.json && ! -e /var/lib/docker && ! -e /var/lib/containerd ]]; then
        install -d -m 755 /etc/docker
        printf '%s\n' '{"storage-driver":"overlay2","features":{"containerd-snapshotter":false}}' \
            > /etc/docker/daemon.json.bazza-next
        mv /etc/docker/daemon.json.bazza-next /etc/docker/daemon.json
    fi
    export DEBIAN_FRONTEND=noninteractive
    apt-get -o Acquire::Retries=3 update
    apt-get -o Acquire::Retries=3 install -y --no-install-recommends \
        ca-certificates curl openssl docker.io docker-compose-v2 docker-buildx
    apt-get clean || true
fi

if ! docker info >/dev/null 2>&1; then
    if [[ $(cat /proc/1/comm) == systemd ]]; then
        systemctl start docker
    else
        # Ubuntu WSL imports also work when systemd is disabled.
        nohup dockerd > dockerd.log 2>&1 </dev/null 9>&- &
    fi
    for ((attempt=0; attempt<60; attempt++)); do
        if docker info >/dev/null 2>&1; then break; fi
        sleep 1
    done
    docker info >/dev/null
fi

if [[ $mode == Setup ]] && ! docker image inspect "$image" >/dev/null 2>&1; then
    if [[ ! -f source/.bazza-revision || $(cat source/.bazza-revision) != "$revision" ]]; then
        curl --fail --silent --show-error --location --retry 3 --connect-timeout 20 --max-time 240 \
            "https://codeload.github.com/attendee-labs/attendee/tar.gz/$revision" -o source.tar.gz
        printf '%s  source.tar.gz\n' "$archive_sha" | sha256sum --check --status
        mkdir -p source
        tar -xzf source.tar.gz --strip-components=1 -C source
        printf '%s\n' "$revision" > source/.bazza-revision
        rm -f source.tar.gz
    fi
    # Keep pip downloads out of image layers without changing the pinned requirements.
    sed 's/\<pip install\>/pip install --no-cache-dir/g' source/Dockerfile \
        | docker build --file - --tag "$image" source
    docker image inspect "$image" >/dev/null
    docker builder prune --all --force || true
fi
docker image inspect "$image" >/dev/null

# Mount the verified WSS startup change on the worker in Setup and Start.
worker_patch_dir=./worker-wss-startup-$revision-v1
worker_patch_refreshed=0
worker_patch_repaired=0
worker_patch_files=(bot_controller.py bot_websocket_client_manager.py bot_websocket_client.py)
worker_patch_valid() (
    cd -- "$1" 2>/dev/null && sha256sum --check --status 2>/dev/null <<'WSS_HASHES'
779d9f5f682a701b9f11dc5c48cb33191805632f8d10bfd223c7bfe70f11952c  bot_controller.py
3dd165c0258955ec7025c8099d7628b16a2e71a2135cbbe6024a3869851d5736  bot_websocket_client_manager.py
944e5187892ab2a28299775c5fa43e38591ab719075d4517c0d1569fea8b7355  bot_websocket_client.py
WSS_HASHES
)
if ! worker_patch_valid "$worker_patch_dir"; then
    worker_patch_stage=$(mktemp -d "$home_dir/.worker-wss-startup.XXXXXX")
    cleanup_worker_patch() {
        local file
        for file in "${worker_patch_files[@]}"; do rm -f -- "$worker_patch_stage/$file"; done
        rmdir -- "$worker_patch_stage" 2>/dev/null || true
    }
    trap cleanup_worker_patch EXIT
    docker run --rm --pull never --network none --read-only --user 0:0 --entrypoint python \
        --mount "type=bind,source=$worker_patch_stage,target=/bazza-worker-patch" \
        -i "$image" - <<'BAZZA_WORKER_WSS_PATCH_PY'
import ast
import hashlib
from pathlib import Path


PIN = "eaacec16f2939b595f196b3957a717bbb0ed23e9"
CONTROLLER = "bots/bot_controller/bot_controller.py"
MANAGER = "bots/bot_controller/bot_websocket_client_manager.py"
CLIENT = "bots/bot_controller/bot_websocket_client.py"
HASHES = {
    CONTROLLER: "d912bb938ddd582f7dae53d749b1fed08c1a9c82d884e2df1cdc1614497422d5",
    MANAGER: "9e993e20fcae1915f462bb7178aaffb5e1721590b782e7031f4d2fdabdae502f",
    CLIENT: "2efe4bad3a83a32ae4c13c9500cc520583945d6c3946704a0c05691b8da370b1",
}


def replace_once(source, old, new):
    if source.count(old) != 1:
        raise ValueError("Pinned WSS startup anchor is missing or ambiguous")
    return source.replace(old, new, 1)


def patch_controller(source):
    return replace_once(
        source,
        '            logger.info("take_action_based_on_bot_in_db - JOINING")\n'
        "            BotEventManager.set_requested_bot_action_taken_at(self.bot_in_db)\n"
        "            self.adapter.init()\n",
        '            logger.info("take_action_based_on_bot_in_db - JOINING")\n'
        "            BotEventManager.set_requested_bot_action_taken_at(self.bot_in_db)\n"
        "            if self.websocket_client_manager and self.get_meeting_type() == MeetingTypes.GOOGLE_MEET:\n"
        "                self.websocket_client_manager.start_per_participant_audio()\n"
        "            self.adapter.init()\n",
    )


def patch_manager(source):
    return replace_once(
        source,
        "    def send_per_participant_audio(self, payload: dict):\n",
        "    def start_per_participant_audio(self):\n"
        "        if self._per_participant_audio_client:\n"
        "            self._ensure_started(self._per_participant_audio_client)\n"
        "\n"
        "    def send_per_participant_audio(self, payload: dict):\n",
    )


def patch_client(source, *, numeric_audit=False):
    cleanup_note = (
        'audit.note("cleanup", self._bazza_audit_client, immediate=True, pending=self.send_queue.qsize())\n'
        if numeric_audit
        else ""
    )
    connected_note = '                audit.note("connected", self._bazza_audit_client, immediate=True)\n' if numeric_audit else ""
    old_error = "type(e).__name__" if numeric_audit else "e"
    changes = [
        (
            '        logger.info("Stopping BotWebsocketClient")\n'
            "        try:\n"
            "            if self.websocket:\n"
            "                self.websocket.close()\n"
            "        except Exception as e:\n"
            f'            logger.error("Error closing BotWebsocketClient websocket: %s", {old_error})\n'
            "        finally:\n"
            "            self.connection_state = self.STOPPED\n"
            + ("            " + cleanup_note if cleanup_note else ""),
            '        logger.info("Stopping BotWebsocketClient")\n'
            "        with self._start_connection_lock:\n"
            "            self.connection_state = self.STOPPED\n"
            "            websocket = self.websocket\n"
            "        try:\n"
            "            if websocket:\n"
            "                websocket.close()\n"
            "        except Exception as e:\n"
            '            logger.error("Error closing BotWebsocketClient websocket: %s", type(e).__name__)\n'
            + ("        " + cleanup_note if cleanup_note else ""),
        ),
        (
            "        with self._start_connection_lock:\n"
            "            if self.connection_state == self.CONNECTING:\n",
            "        with self._start_connection_lock:\n"
            "            if self.connection_state == self.STOPPED:\n"
            "                return\n"
            "            if self.connection_state == self.CONNECTING:\n",
        ),
        (
            "                self.websocket = connect(self.websocket_url)\n",
            "                websocket = connect(self.websocket_url)\n",
        ),
        (
            "                self.connection_state = self.CONNECTED\n" + connected_note,
            "                with self._start_connection_lock:\n"
            "                    stopped = self.connection_state == self.STOPPED\n"
            "                    if not stopped:\n"
            "                        self.websocket = websocket\n"
            "                        self.connection_state = self.CONNECTED\n"
            "                if stopped:\n"
            "                    websocket.close()\n"
            "                    return\n" + connected_note,
        ),
        (
            "            except Exception as e:\n"
            "                retries += 1\n",
            "            except Exception as e:\n"
            "                if self.connection_state == self.STOPPED:\n"
            "                    return\n"
            "                retries += 1\n",
        ),
        (
            "        # Handle case where we were stopped before we could connect\n"
            "        if self.connection_state != self.CONNECTING:\n"
            '            logger.info("BotWebsocketClient connection loop exited because connection state is %s", self.connection_state)\n'
            "            return\n"
            "\n"
            "        # Exhausted retries\n"
            "        self.connection_state = self.FAILED\n",
            "        with self._start_connection_lock:\n"
            "            if self.connection_state != self.CONNECTING:\n"
            "                return\n"
            "            self.connection_state = self.FAILED\n",
        ),
    ]
    if not numeric_audit:
        changes.extend(
            [
                (
                    '        logger.info(f"Starting BotWebsocketClient for url {self.websocket_url}")\n',
                    '        logger.info("Starting BotWebsocketClient")\n',
                ),
                (
                    "                    self._max_retries,\n"
                    "                    e,\n",
                    "                    self._max_retries,\n"
                    "                    type(e).__name__,\n",
                ),
            ]
        )
    for old, new in changes:
        source = replace_once(source, old, new)
    return source


def check_sources(sources):
    normalized = {name: sources[name].replace("\r\n", "\n") for name in HASHES}
    for name, source in normalized.items():
        if hashlib.sha256(source.encode("utf-8")).hexdigest() != HASHES[name]:
            raise ValueError(f"{name} does not match Attendee {PIN}")
    return normalized


def build_overlay(sources):
    normalized = check_sources(sources)
    patches = {CONTROLLER: patch_controller, MANAGER: patch_manager, CLIENT: patch_client}
    output = {name: patch(normalized[name]) for name, patch in patches.items()}
    for name, source in output.items():
        ast.parse(source, filename=name)
    return output
upstream = Path("/attendee")
output = Path("/bazza-worker-patch")
files = build_overlay({name: (upstream / name).read_text(encoding="utf-8") for name in HASHES})
for name, contents in files.items():
    destination = output / Path(name).name
    with destination.open("xb") as stream:
        stream.write(contents.encode("utf-8"))
    destination.chmod(0o644)
BAZZA_WORKER_WSS_PATCH_PY
    worker_patch_valid "$worker_patch_stage"
    if [[ -d $worker_patch_dir ]]; then
        for file in "${worker_patch_files[@]}"; do
            mv -f -- "$worker_patch_stage/$file" "$worker_patch_dir/$file"
        done
        rmdir -- "$worker_patch_stage"
        worker_patch_repaired=1
    else
        chmod 755 "$worker_patch_stage"
        mv -T -- "$worker_patch_stage" "$worker_patch_dir"
    fi
    trap - EXIT
    worker_patch_refreshed=1
fi

if [[ ! -s credentials.env ]]; then
    {
        printf 'DJANGO_SECRET_KEY=%s\n' "$(openssl rand -hex 48)"
        printf 'CREDENTIALS_ENCRYPTION_KEY=%s\n' "$(openssl rand -base64 32 | tr '/+' '_-')"
        printf 'POSTGRES_PASSWORD=%s\n' "$(openssl rand -hex 32)"
    } > credentials.env.next
    mv credentials.env.next credentials.env
fi

if [[ ! -s ca.pem || ! -s ca.key ]] || ! openssl x509 -checkend 2592000 -noout -in ca.pem; then
    openssl req -x509 -newkey rsa:3072 -nodes -sha256 -days 3650 \
        -subj '/CN=Bazza Attendee Local CA' -keyout ca.key.next -out ca.pem.next \
        -addext 'basicConstraints=critical,CA:TRUE' -addext 'keyUsage=critical,keyCertSign,cRLSign'
    mv ca.key.next ca.key
    mv ca.pem.next ca.pem
fi
if [[ ! -s callback.pem || ! -s callback.key ]] \
    || ! openssl x509 -checkend 2592000 -noout -in callback.pem \
    || ! openssl verify -CAfile ca.pem callback.pem; then
    printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:bazza-attendee.local\n' > callback.ext
    openssl req -new -newkey rsa:2048 -nodes -subj '/CN=bazza-attendee.local' \
        -keyout callback.key.next -out callback.csr
    openssl x509 -req -sha256 -days 730 -in callback.csr -CA ca.pem -CAkey ca.key \
        -CAcreateserial -extfile callback.ext -out callback.pem.next
    mv callback.key.next callback.key
    mv callback.pem.next callback.pem
    rm -f callback.csr callback.ext
fi
install -d -m 755 container-config
if [[ ! -s container-config/callback-ca-bundle.pem ]] || ! cmp -s ca.pem installed-ca.pem; then
    docker run --rm --entrypoint cat "$image" /etc/ssl/certs/ca-certificates.crt > container-config/callback-ca-bundle.pem.next
    cat ca.pem >> container-config/callback-ca-bundle.pem.next
    chmod 644 container-config/callback-ca-bundle.pem.next
    mv container-config/callback-ca-bundle.pem.next container-config/callback-ca-bundle.pem
    cp ca.pem installed-ca.pem
fi

cat > container-config/bazza_settings.py <<'PY'
import os
from attendee.settings.development import *

DEBUG = False
DATABASES["default"]["PASSWORD"] = os.environ["POSTGRES_PASSWORD"]
PY
chmod 644 container-config/bazza_settings.py

# Only the API is published, on loopback. The worker can initiate a callback
# to the Windows WSL NAT adapter; the private TLS keys stay out of containers.
cat > compose.yaml <<'YAML'
name: bazza-attendee-local
x-attendee: &attendee
  image: ${BAZZA_IMAGE:?}
  env_file: credentials.env
  environment:
    POSTGRES_HOST: postgres
    REDIS_URL: redis://redis:6379/5
    DJANGO_SETTINGS_MODULE: bazza_settings
    ALLOWED_HOSTS: 127.0.0.1,localhost
    ENABLE_CHROME_SANDBOX: 'false'
    DISABLE_SIGNUP: 'true'
    AWS_DEFAULT_REGION: us-east-1
    AWS_EC2_METADATA_DISABLED: 'true'
    CONCURRENT_BOTS_LIMIT: '1'
    ENABLE_BOT_WEBSOCKET_PORT_SELECTION_BY_OS: 'true'
    MASK_TRANSCRIPT_IN_LOGS: 'true'
    PYTHONPATH: /etc/bazza-attendee
    SSL_CERT_FILE: /etc/bazza-attendee/callback-ca-bundle.pem
  volumes:
    - ./container-config:/etc/bazza-attendee:ro
  restart: unless-stopped

services:
  api:
    <<: *attendee
    entrypoint: []
    command: [python, manage.py, runserver, '0.0.0.0:8000', --noreload]
    ports:
      - '127.0.0.1:${BAZZA_API_PORT:?}:8000'
  worker:
    <<: *attendee
    command: [celery, -A, attendee, worker, -l, WARNING, --concurrency=1, --prefetch-multiplier=1]
    extra_hosts:
      bazza-attendee.local: ${BAZZA_WINDOWS_HOST:?}
    shm_size: 1gb
    volumes:
      - ./container-config:/etc/bazza-attendee:ro
      - type: bind
        source: ${BAZZA_WSS_PATCH_DIR:?}/bot_controller.py
        target: /attendee/bots/bot_controller/bot_controller.py
        read_only: true
        bind:
          create_host_path: false
      - type: bind
        source: ${BAZZA_WSS_PATCH_DIR:?}/bot_websocket_client_manager.py
        target: /attendee/bots/bot_controller/bot_websocket_client_manager.py
        read_only: true
        bind:
          create_host_path: false
      - type: bind
        source: ${BAZZA_WSS_PATCH_DIR:?}/bot_websocket_client.py
        target: /attendee/bots/bot_controller/bot_websocket_client.py
        read_only: true
        bind:
          create_host_path: false
  postgres:
    image: postgres:15.3-alpine
    environment:
      POSTGRES_DB: attendee_development
      POSTGRES_USER: attendee_development_user
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:?}
    volumes:
      - postgres:/var/lib/postgresql/data
    restart: unless-stopped
  redis:
    image: redis:7-alpine
    volumes:
      - redis:/data
    restart: unless-stopped

volumes:
  postgres:
  redis:
YAML

printf 'BAZZA_IMAGE=%s\nBAZZA_WINDOWS_HOST=%s\nBAZZA_API_PORT=%s\nBAZZA_WSS_PATCH_DIR=%s\n' \
    "$image" "$windows_host" "$api_port" "$worker_patch_dir" > runtime.env.next
mv runtime.env.next runtime.env
compose() { docker compose --env-file credentials.env --env-file runtime.env "$@"; }

compose up -d postgres redis
for ((attempt=0; attempt<60; attempt++)); do
    if compose exec -T postgres pg_isready -h 127.0.0.1 -U attendee_development_user -d attendee_development >/dev/null \
        && compose exec -T redis redis-cli ping | grep -x PONG >/dev/null; then break; fi
    sleep 1
done
compose exec -T postgres pg_isready -h 127.0.0.1 -U attendee_development_user -d attendee_development >/dev/null
compose exec -T redis redis-cli ping | grep -x PONG >/dev/null

if [[ $mode == Setup && ! -f initialized ]]; then
    compose run --rm --no-deps api python manage.py migrate --noinput
    compose run --rm --no-deps -T api python manage.py shell -i python <<'PY' | grep '^BAZZA_LOCAL_KEY=' | cut -d= -f2 > api-key.next
from accounts.models import Organization
from bots.models import ApiKey, Project
from django.db import transaction

with transaction.atomic():
    org, _ = Organization.objects.get_or_create(name="Bazza Local", defaults={"centicredits": 1000000})
    project, _ = Project.objects.get_or_create(name="Bazza Local", organization=org)
    ApiKey.objects.filter(project=project, name="Bazza Desktop").delete()
    _, key = ApiKey.create(project=project, name="Bazza Desktop")

print("BAZZA_LOCAL_KEY=" + key)
PY
    grep -Exq '[[:alnum:]]{32}' api-key.next
    mv api-key.next api-key
fi
test -s api-key
if (( worker_patch_repaired )); then
    compose up -d api
    compose up -d --no-deps --force-recreate worker
else
    compose up -d api worker
fi

if [[ $mode == Setup ]] || (( worker_patch_refreshed )); then
    for ((attempt=0; attempt<12; attempt++)); do
        if compose exec -T worker celery -A attendee inspect ping --timeout 3 2>/dev/null | grep pong >/dev/null; then
            break
        fi
        sleep 1
    done
    [[ $attempt -lt 12 ]]
fi

# The Windows setup script temporarily holds the callback listener open.
if [[ $mode == Setup ]]; then
    compose exec -T worker python -c \
        'import socket, sys; socket.create_connection(("bazza-attendee.local", int(sys.argv[1])), timeout=5).close()' \
        "$callback_port" || exit 11
fi
touch initialized
