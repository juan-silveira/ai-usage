#!/usr/bin/env python3
"""Configure the Codex lamp as a per-user macOS login service."""

import argparse
from getpass import getpass
import os
from pathlib import Path
import plistlib
import stat
import subprocess
import sys
import tempfile
from urllib.parse import urlsplit

LABEL = "io.github.ai-usage.codex-lamp"
LAUNCHCTL = "/bin/launchctl"
PYTHON = "/usr/bin/python3"
USER_LIBRARY = Path.home() / "Library"
SUPPORT = USER_LIBRARY / "Application Support" / "AI Usage Lamp"
PLIST = USER_LIBRARY / "LaunchAgents" / f"{LABEL}.plist"
PLIST_DRAFT = SUPPORT / f"{LABEL}.plist"
WEBHOOK = SUPPORT / "webhook-url"
BRIDGE = SUPPORT / "codex-lamp.py"
LOG = SUPPORT / "events.log"
DOMAIN = f"gui/{os.getuid()}"
JOB = f"{DOMAIN}/{LABEL}"


def write_private(path, content):
    fd, name = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as file:
            file.write(content)
        os.replace(name, path)
    finally:
        Path(name).unlink(missing_ok=True)


def prepare(port):
    SUPPORT.mkdir(mode=0o700, parents=True, exist_ok=True)
    owner = SUPPORT.lstat()
    if not stat.S_ISDIR(owner.st_mode) or owner.st_uid != os.getuid():
        raise RuntimeError("A pasta do serviço precisa pertencer à sua conta.")
    SUPPORT.chmod(0o700)
    PLIST.parent.mkdir(exist_ok=True)
    write_private(BRIDGE, Path(__file__).with_name("codex-lamp.py").read_bytes())
    if not LOG.exists():
        write_private(LOG, b"")
    if os.path.lexists(PLIST):
        if not PLIST.is_file() or PLIST.is_symlink():
            raise RuntimeError("Já existe um item incompatível no local do serviço.")
        with PLIST.open("rb") as file:
            current = plistlib.load(file)
        if current.get("Label") != LABEL or str(BRIDGE) not in current.get("ProgramArguments", []):
            raise RuntimeError("Já existe outro serviço usando esse identificador.")
    write_private(PLIST_DRAFT, plistlib.dumps({
        "Label": LABEL,
        "ProgramArguments": [
            PYTHON, "-u", str(BRIDGE), "--port", str(port), "--webhook-file", str(WEBHOOK),
        ],
        "RunAtLoad": True,
        "KeepAlive": True,
        "ThrottleInterval": 15,
        "StandardOutPath": str(LOG),
        "StandardErrorPath": str(LOG),
        "ProcessType": "Background",
    }))


def activate():
    if not sys.stdin.isatty():
        raise RuntimeError("Abra o Terminal para configurar a URL em um campo oculto.")
    url = getpass("Cole a URL privada do Voice Monkey e pressione Enter: ").strip()
    parts = urlsplit(url)
    if (
        parts.scheme != "https"
        or parts.netloc != "api-v3.voicemonkey.io"
        or len(parts.path.strip("/").split("/")) != 3
        or not parts.path.startswith("/catch/")
        or parts.fragment
        or any(ch.isspace() for ch in url)
    ):
        raise RuntimeError("Cole a URL criada em Inbound Webhooks, que começa com https://api-v3.voicemonkey.io/catch/.")
    write_private(WEBHOOK, url.encode("utf-8"))
    write_private(PLIST, PLIST_DRAFT.read_bytes())

    subprocess.run([LAUNCHCTL, "enable", JOB], check=True)
    running = subprocess.run(
        [LAUNCHCTL, "print", JOB], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    if running.returncode == 0:
        subprocess.run([LAUNCHCTL, "kickstart", "-k", JOB], check=True)
    else:
        subprocess.run([LAUNCHCTL, "bootstrap", DOMAIN, str(PLIST)], check=True)
    print("Serviço da luminária ativado. Você já pode fechar o Terminal.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prepare", action="store_true", help="Prepara o serviço sem pedir a URL ou iniciá-lo.")
    parser.add_argument("--port", type=int, default=47821)
    args = parser.parse_args()
    prepare(args.port)
    if args.prepare:
        print("Serviço preparado. Falta cadastrar a URL do Voice Monkey para ativá-lo.")
    else:
        activate()


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, subprocess.CalledProcessError) as exc:
        print(f"Não foi possível configurar o serviço. {exc}", file=sys.stderr)
        sys.exit(1)
    except ValueError:
        print("Não foi possível ler a configuração ou a URL do webhook.", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
