# Google Meet recording with Attendee

Bazza can ask an Attendee instance you control to join a Google Meet call as **Bazza Recorder**. The visible bot sends per-participant audio over TLS to the desktop app. Bazza stores the audio and participant names in the local meeting folder. Enable the integration for each recording in the picker.

## Set up Google Meet on Windows x64

1. In Bazza, open **Settings → Google Meet** and select **Set up Google Meet**.
2. If Windows asks for administrator approval to enable WSL 2, approve it. If Windows needs a restart, reopen Bazza afterward and select **Set up Google Meet** again. [Microsoft WSL installation guide](https://learn.microsoft.com/windows/wsl/install).
3. Wait for **Google Meet is ready**. The first setup needs internet access and time to download and build the local service. Use **Repair Google Meet setup** if you need to run the setup again.

Bazza creates a dedicated Ubuntu environment in WSL 2 on the same computer, installs Docker Engine inside it, and builds Attendee from a pinned upstream revision. Setup downloads Ubuntu and its packages, Attendee source, and container images and build dependencies. A fresh Docker installation uses `overlay2` and clears unused build cache after the image is ready. Repair keeps the storage driver of an existing installation. Bazza generates the API key, local TLS certificate and callback configuration automatically. [Ubuntu WSL images](https://cloud-images.ubuntu.com/wsl/), [Docker image storage](https://docs.docker.com/engine/storage/containerd/#disk-space-usage), [Attendee source](https://github.com/attendee-labs/attendee/tree/eaacec16f2939b595f196b3957a717bbb0ed23e9).

When you start a recording with Google Meet enabled, Bazza starts the local service as needed and refreshes the callback address. The managed setup uses the default WSL 2 NAT network. The Attendee API listens on loopback, and the audio callback uses the Windows WSL adapter. The managed service is for Bazza on this computer. [Microsoft WSL networking](https://learn.microsoft.com/windows/wsl/networking).

The managed worker starts the participant audio connection when the bot begins
joining. Bazza verifies the worker patch against the pinned Attendee source
locally and checks the worker after installing or repairing the patch.

On macOS or with your own Attendee instance, use the advanced setup below. To switch from an existing manual connection to managed setup on Windows, first select **Disconnect Google Meet**.

## Advanced setup with your own Attendee instance

### Prepare the Attendee instance

Run the API and at least one worker with access to Google Meet. The Attendee development Compose stack runs the API on port `8000` and uses a separate worker, PostgreSQL and Redis. Create an API key in the instance's **API Keys** page after its database is ready. For a local instance, publish the API only on the host loopback interface when possible. The pilot used Attendee 1.89.2. [Attendee self-hosting instructions](https://github.com/attendee-labs/attendee#running-in-development-mode), [pilot environment](test-reports/2026-11-03-attendee-google-meet-pilot.md).

The API and audio connections go in different directions. Bazza calls the API to manage the bot and resolve participant names. The Attendee worker connects back to Bazza to deliver the audio. The worker needs a route to the desktop listener, and it must trust the TLS certificate for the hostname in the callback URL. [Per-participant audio protocol](https://docs.attendee.dev/guides/realtimeaudio#per-participant-audio-streaming).

### Configure Bazza

Open **Settings → Google Meet → Advanced settings · use your own server** and enter these values for your installation.

| Field | Value |
| --- | --- |
| API URL | Attendee base URL, such as `http://127.0.0.1:8000` for an API on the same computer. An `/api/v1` suffix is optional. Use HTTPS for a remote instance. |
| API key | The key from your Attendee instance. Bazza saves it in the system credential store. |
| Callback URL | A `wss://` URL reachable from the worker, such as `wss://bazza-attendee.local:18761/audio`. Bazza generates a separate access token for each recording. Enter the URL without a query or fragment. |
| Bind address | A local address and port where Bazza listens, such as `172.26.112.1:18761` on a Windows WSL adapter. Use the address on your computer. |
| TLS certificate and key | Absolute paths readable by Bazza to the PEM server certificate and its private key. The certificate must cover the callback hostname. Give the worker the issuing CA certificate, and keep the private key on the Bazza machine. |

### Local callback certificate

If you need a certificate for a private callback hostname, run the following with OpenSSL 3 in a private directory. Replace `bazza-attendee.local` with the hostname you will enter in the callback URL.

```sh
umask 077
callback_host=bazza-attendee.local
openssl req -x509 -newkey rsa:2048 -noenc -days 30 \
  -subj '/CN=Bazza callback CA' -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign' \
  -keyout bazza-ca-key.pem -out bazza-local-ca.pem
openssl req -new -newkey rsa:2048 -noenc -subj "/CN=$callback_host" \
  -keyout bazza-server-key.pem -out bazza-server.csr
openssl req -x509 -in bazza-server.csr -CA bazza-local-ca.pem -CAkey bazza-ca-key.pem \
  -days 30 -addext "subjectAltName=DNS:$callback_host" \
  -addext 'extendedKeyUsage=serverAuth' -out bazza-server.pem
openssl verify -purpose sslserver -verify_hostname "$callback_host" \
  -CAfile bazza-local-ca.pem bazza-server.pem
```

Bazza uses `bazza-server.pem` and `bazza-server-key.pem`. Add `bazza-local-ca.pem` to the worker's trust bundle. Keep both private keys outside source control and the worker's mounted files. [OpenSSL certificate commands](https://docs.openssl.org/3.0/man1/openssl-req/).

### Worker in WSL 2 on Windows

For Docker Engine inside WSL 2 with default NAT, run `ip -4 route show default` inside WSL and use its gateway as the Windows host address. Bind Bazza to that address on Windows. In the worker's Compose configuration, map your callback hostname to the same Windows address. The Docker `host-gateway` in this topology is on the Linux side. Check the Windows address again after a WSL restart if the connection stops working. [Microsoft WSL networking](https://learn.microsoft.com/windows/wsl/networking).

For example, save this Compose override as `bazza.override.yaml` next to the Attendee development Compose file. Replace the example IP with your WSL gateway. `bazza-ca-bundle.pem` combines the worker's normal trusted roots with the CA that issued Bazza's callback certificate. Keep any worker-specific CA trust settings when merging the bundle.

```yaml
services:
  attendee-worker-local:
    extra_hosts:
      - "bazza-attendee.local:172.26.112.1"
    environment:
      SSL_CERT_FILE: /etc/bazza-ca-bundle.pem
    volumes:
      - ./bazza-ca-bundle.pem:/etc/bazza-ca-bundle.pem:ro
```

For an unmodified development worker that is already running, copy its trust bundle and append your local CA before applying the override.

```sh
docker compose -f dev.docker-compose.yaml exec -T attendee-worker-local \
  cat /etc/ssl/certs/ca-certificates.crt > bazza-ca-bundle.pem
cat bazza-local-ca.pem >> bazza-ca-bundle.pem
docker compose -f dev.docker-compose.yaml -f bazza.override.yaml up -d attendee-worker-local
```

Use the same callback hostname in the certificate and in Bazza. If the worker times out, check its hostname resolution, the listener's bind address and whether the desktop firewall permits the worker on that interface. The listener is active during a recording with Google Meet enabled. On Windows, the pilot's Attendee worker image reached a TLS test listener through this route with hostname and CA verification. [Development Compose service](https://github.com/attendee-labs/attendee/blob/eaacec16f2939b595f196b3957a717bbb0ed23e9/dev.docker-compose.yaml), [container CA trust](https://docs.docker.com/engine/network/ca-certs/).

### Worker in Docker Desktop on macOS or Windows

Docker Desktop provides `host.docker.internal` for connections from a container to the desktop host. Use that name in the callback and its TLS certificate, or map a private name to the host address the worker can reach. Bind Bazza on the desktop interface used by the route and apply CA trust inside the worker. [Docker Desktop host access](https://docs.docker.com/desktop/features/networking/networking-how-tos/).

## Start and find the tracks

1. Open **New recording**, choose **Google Meet** and paste the link to the call. Start recording and let **Bazza Recorder** in if Meet shows a join request. Bazza reports the bot's progress separately from the local recording.
2. During recording, the installed local Whisper model produces live captions from the separate audio streams. The meeting view and recording bar label each speaker with the display name reported by Attendee. Captions for a speaker wait for their name to arrive. Pause suspends collection for the local tracks and named captions while the bot stays in the meeting. Stop ends local collection and asks Attendee to remove the bot. If Attendee becomes unreachable, use Meet's participant controls or the Attendee instance to remove a bot that is still in the call.
3. Open the completed meeting in the library to find the named tracks and open each WAV. The local `attendee/` folder contains 16 kHz mono PCM files such as `participant-001.wav`. `tracks.json` maps each WAV to the participant ID and display name returned by Attendee. Participants with matching names can still have distinct tracks. Bazza shows a fallback label when metadata is unavailable. [Google Meet admission controls](https://support.google.com/meet/answer/9302870?hl=en).
4. Completed live rows appear as the first transcript version with their display names. To recognize the saved WAVs again, choose **New transcript version** and select **Google Meet participant tracks**, a local Whisper model, and a language. The result is a separate version and its timestamps may be approximate. The main player uses the local recording mix.

Bazza requests separate audio with Attendee's recording and asynchronous audio chunks disabled. The Attendee bot also uses Google Meet's native captions and your instance may retain them. Bazza uses the audio stream and participant metadata from Attendee. [Bazza privacy](../privacy.md).

The tracks use Attendee's stream timestamps to place packets on an approximate timeline. Each recording can store up to 64 participant tracks, eight hours per track, and 2 GiB of WAV sample data including padding. Bazza keeps the available tracks and reports an error when it reaches a limit. The pilot exercised audio delivery and real Meet admission with a separate receiver before the desktop integration. Its synchronization and intelligibility observations are recorded separately. [Pilot evidence and limitations](test-reports/2026-11-03-attendee-google-meet-pilot.md).
