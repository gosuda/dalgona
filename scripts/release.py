#!/usr/bin/env python3
"""Implement the standalone release scripts using Cargo metadata."""

import json
import re
import shutil
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:
    sys.exit("scripts/release.py requires Python 3.11 or newer (tomllib)")
from dataclasses import dataclass
from pathlib import Path

PUBLISH_USAGE = (
    "usage: scripts/publish-crates.sh [--dry-run] [--check-registry-deps] "
    "[--check-dep-only <name> <req>] <workspace-root>"
)
SEMVER_USAGE = "usage: scripts/semver-gate.sh [--baseline-root <dir>] <workspace-root>"
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"



@dataclass(frozen=True, slots=True)
class Member:
    name: str
    version: str
    manifest_path: Path
    publishable: bool


@dataclass(frozen=True, slots=True)
class Dependency:
    package: str
    name: str
    path: Path | None
    source: str | None
    requirement: str | None


@dataclass(frozen=True, slots=True)
class Metadata:
    members: tuple[Member, ...]
    dependencies: tuple[Dependency, ...]


def mapping(value: object) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ValueError("cargo metadata returned an invalid metadata object")
    result: dict[str, object] = {}
    for key, item in value.items():
        if not isinstance(key, str):
            raise ValueError("cargo metadata returned a non-string object key")
        result[key] = item
    return result


def sequence(value: object) -> list[object]:
    if not isinstance(value, list):
        raise ValueError("cargo metadata returned an invalid metadata list")
    return [item for item in value]


def string_field(fields: dict[str, object], name: str) -> str:
    value = fields.get(name)
    if not isinstance(value, str):
        raise ValueError(f"cargo metadata returned an invalid {name} field")
    return value


def optional_string_field(fields: dict[str, object], name: str) -> str | None:
    value = fields.get(name)
    if value is None:
        return None
    if not isinstance(value, str):
        raise ValueError(f"cargo metadata returned an invalid {name} field")
    return value


def metadata_value(value: object) -> Metadata:
    fields = mapping(value)
    members: list[Member] = []
    for item in sequence(fields.get("members")):
        member = mapping(item)
        publishable = member.get("publishable")
        if not isinstance(publishable, bool):
            raise ValueError("cargo metadata returned an invalid publishable field")
        members.append(
            Member(
                string_field(member, "name"),
                string_field(member, "version"),
                Path(string_field(member, "manifest_path")),
                publishable,
            )
        )
    dependencies: list[Dependency] = []
    for item in sequence(fields.get("dependencies")):
        dependency = mapping(item)
        path = optional_string_field(dependency, "path")
        dependencies.append(
            Dependency(
                string_field(dependency, "package"),
                string_field(dependency, "name"),
                Path(path) if path is not None else None,
                optional_string_field(dependency, "source"),
                optional_string_field(dependency, "req"),
            )
        )
    return Metadata(tuple(members), tuple(dependencies))


def error(message: str, code: int) -> int:
    print(message, file=sys.stderr)
    return code


def require_curl() -> int | None:
    return (
        None
        if shutil.which("curl")
        else error("curl is required to reach the crates.io sparse index", 64)
    )


def usage_error(message: str) -> int:
    return error(message, 64)


def parse_root(root: str) -> tuple[Path | None, int | None]:
    manifest = Path(root) / "Cargo.toml"
    if not manifest.is_file():
        return None, error(f"no Cargo.toml at {root}", 64)
    return Path(root).absolute(), None


def workspace_filter(raw: object) -> dict[str, object]:
    fields = mapping(raw)
    members: set[object] = set()
    for member in sequence(fields.get("workspace_members")):
        if not isinstance(member, str):
            raise ValueError("cargo metadata returned a non-string workspace member")
        members.add(member)
    packages = [
        mapping(item)
        for item in sequence(fields.get("packages"))
        if isinstance(mapping(item).get("id"), str) and mapping(item).get("id") in members
    ]
    dependencies: list[dict[str, object]] = []
    for package in packages:
        for item in sequence(package.get("dependencies")):
            dependency = mapping(item)
            if dependency.get("kind") == "dev":
                continue
            dependencies.append(
                {
                    "package": package.get("name"),
                    "name": dependency.get("name"),
                    "path": dependency.get("path"),
                    "source": dependency.get("source"),
                    "req": dependency.get("req"),
                }
            )
    return {
        "members": [
            {
                "name": package.get("name"),
                "version": package.get("version"),
                "manifest_path": package.get("manifest_path"),
                "publishable": not (
                    (publish := package.get("publish")) is False or publish == []
                ),
            }
            for package in packages
        ],
        "dependencies": dependencies,
    }


def run_metadata(root: Path, metadata_error: str) -> tuple[Metadata | None, int | None]:
    cargo = subprocess.run(
        [
            "cargo",
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
            str(root / "Cargo.toml"),
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if cargo.returncode:
        sys.stdout.write(cargo.stdout)
        sys.stderr.write(cargo.stderr)
        return None, cargo.returncode
    try:
        return metadata_value(workspace_filter(json.loads(cargo.stdout))), None
    except (json.JSONDecodeError, ValueError):
        return None, error(metadata_error, 2)


def package_directories(members: tuple[Member, ...]) -> dict[Path, str]:
    return {member.manifest_path.parent.resolve(): member.name for member in members}


def dependency_target(
    dependency: Dependency,
    package_directories_by_name: dict[str, Path],
    directory_names: dict[Path, str],
) -> str | None:
    if dependency.path is None:
        return None
    target_path = dependency.path
    if not target_path.is_absolute():
        package_directory = package_directories_by_name.get(dependency.package)
        if package_directory is not None:
            target_path = package_directory / target_path
    return directory_names.get(target_path.resolve())


def sparse_path(name: str) -> str:
    if len(name) <= 3:
        return f"3/{name[0]}/{name}"
    if len(name) == 4:
        return f"2/{name[:2]}/{name}"
    return f"{name[:2]}/{name[2:4]}/{name}"


def sparse_index(name: str) -> str:
    try:
        result = subprocess.run(
            [
                "curl",
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                f"https://index.crates.io/{sparse_path(name)}",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
    except OSError:
        return ""
    return result.stdout if result.returncode == 0 else ""


def index_versions(index_text: str) -> list[str]:
    versions: list[str] = []
    for line in index_text.splitlines():
        if not line:
            continue
        try:
            item = json.loads(line)
        except json.JSONDecodeError:
            return []
        if not isinstance(item, dict) or not isinstance(item.get("vers"), str):
            return []
        versions.append(item["vers"])
    return versions


def satisfies_requirement(version: str, requirement: str) -> bool:
    exact = requirement.startswith("=")
    if exact:
        requirement = requirement[1:]
    req = re.fullmatch(r"0\.([0-9]+)(?:\.([0-9]+))?", requirement)
    candidate = re.fullmatch(r"([0-9]+)\.([0-9]+)\.([0-9]+)", version)
    if req is None or candidate is None:
        return False
    req_minor = int(req.group(1))
    req_patch = int(req.group(2) or "0")
    version_major, version_minor, version_patch = map(int, candidate.groups())
    if version_major != 0 or version_minor != req_minor:
        return False
    return version_patch == req_patch if exact else version_patch >= req_patch


def check_dep_only(name: str, requirement: str) -> int:
    if any(satisfies_requirement(version, requirement) for version in index_versions(sys.stdin.read())):
        return 0
    return error(
        f'dalgon dependency {name} "{requirement}" not on crates.io; release dalgon first',
        3,
    )


def publish_order(members: tuple[Member, ...], dependencies: tuple[Dependency, ...]) -> list[str] | None:
    directories_by_name = {member.name: member.manifest_path.parent.resolve() for member in members}
    names_by_directory = package_directories(members)
    publishable_names = {member.name for member in members if member.publishable}
    prerequisites: dict[str, set[str]] = {name: set() for name in publishable_names}
    for dependency in dependencies:
        package = dependency.package
        target = dependency_target(dependency, directories_by_name, names_by_directory)
        if package in publishable_names and target in publishable_names:
            prerequisites[package].add(target)
    remaining = set(publishable_names)
    ordered: list[str] = []
    while remaining:
        ready = sorted(name for name in remaining if not (prerequisites[name] & remaining))
        if not ready:
            return None
        selected = ready[0]
        ordered.append(selected)
        remaining.remove(selected)
    return ordered


def publish_args(
    arguments: list[str],
) -> tuple[bool, bool, str | None, str | None, str] | None:
    dry_run = False
    check_registry_deps = False
    dep_name: str | None = None
    dep_req: str | None = None
    root: str | None = None
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--dry-run":
            dry_run = True
            index += 1
        elif argument == "--check-registry-deps":
            check_registry_deps = True
            index += 1
        elif argument == "--check-dep-only":
            if dep_name is not None or index + 2 >= len(arguments):
                return None
            dep_name, dep_req = arguments[index + 1 : index + 3]
            index += 3
        elif argument.startswith("-"):
            return None
        elif root is None:
            root = argument
            index += 1
        else:
            return None
    if root is None:
        return None
    return dry_run, check_registry_deps, dep_name, dep_req, root


def publish_main(arguments: list[str]) -> int:
    parsed = publish_args(arguments)
    if parsed is None:
        return usage_error(PUBLISH_USAGE)
    dry_run, check_registry_deps, dep_name, dep_req, root_arg = parsed
    root, status = parse_root(root_arg)
    if status is not None or root is None:
        return status or 64
    if dep_name is not None:
        if dep_req is None:
            return usage_error(PUBLISH_USAGE)
        return check_dep_only(dep_name, dep_req)

    try:
        manifest: object = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
        workspace = mapping(manifest).get("workspace")
        workspace_package = mapping(workspace).get("package") if workspace is not None else None
        workspace_version = (
            optional_string_field(mapping(workspace_package), "version")
            if workspace_package is not None
            else None
        )
    except (OSError, tomllib.TOMLDecodeError, ValueError):
        workspace_version = None
    if not workspace_version:
        return error(
            f"workspace root {root_arg} has no [workspace.package] version; lockstep is broken",
            2,
        )
    metadata, status = run_metadata(root, "the cargo metadata shape is invalid for the publish order")
    if status is not None or metadata is None:
        return status or 2
    for member in metadata.members:
        if member.version != workspace_version:
            return error(
                f"workspace member {member.name} does not inherit the workspace version; lockstep is broken",
                2,
            )
    publishable = [member.name for member in metadata.members if member.publishable]
    if not publishable:
        return error(f"no publishable member found in {root_arg}", 4)

    directories_by_name = {member.name: member.manifest_path.parent.resolve() for member in metadata.members}
    names_by_directory = package_directories(metadata.members)
    workspace_names = {member.name for member in metadata.members}
    if check_registry_deps:
        for dependency in metadata.dependencies:
            target = dependency_target(dependency, directories_by_name, names_by_directory)
            if dependency.path is not None and target is None:
                return error(
                    f"workspace member {dependency.package} depends on {dependency.name} by path; "
                    "dalgona builds only against published dal-* crates",
                    2,
                )
            if dependency.name not in workspace_names and dependency.name.startswith(("dal-", "dalgon")) and dependency.source != CRATES_IO_SOURCE:
                return error(
                    f"workspace member {dependency.package} depends on {dependency.name} outside crates.io; "
                    "dalgona builds only against published dal-* crates",
                    2,
                )

    order = publish_order(metadata.members, metadata.dependencies)
    if order is None:
        return error("dependency cycle among workspace members; no publish order exists", 2)

    if check_registry_deps and not dry_run:
        status = require_curl()
        if status is not None:
            return status
        for dependency in metadata.dependencies:
            if dependency.name in workspace_names or not dependency.name.startswith(("dal-", "dalgon")) or dependency.source != CRATES_IO_SOURCE:
                continue
            requirement = dependency.requirement or ""
            versions = index_versions(sparse_index(dependency.name))
            if not any(satisfies_requirement(version, requirement) for version in versions):
                return error(
                    f'dalgon dependency {dependency.name} "{requirement}" not on crates.io; release dalgon first',
                    3,
                )

    if dry_run:
        for name in order:
            print(f"cargo publish -p {name}")
        return 0
    for name in order:
        published = subprocess.run(
            ["cargo", "publish", "-p", name, "--locked", "--all-features"],
            cwd=root,
            check=False,
        )
        if published.returncode:
            return error(f"cargo publish failed for {name}; see the output above", 5)
    return 0


def semver_args(arguments: list[str]) -> tuple[str | None, str] | None:
    baseline: str | None = None
    root: str | None = None
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "--baseline-root":
            if baseline is not None or index + 1 >= len(arguments):
                return None
            baseline = arguments[index + 1]
            index += 2
        elif argument.startswith("-"):
            return None
        elif root is None:
            root = argument
            index += 1
        else:
            return None
    if root is None:
        return None
    return baseline, root


def semver_main(arguments: list[str]) -> int:
    parsed = semver_args(arguments)
    if parsed is None:
        return usage_error(SEMVER_USAGE)
    baseline_arg, root_arg = parsed
    curl_status = require_curl()
    if curl_status is not None:
        return curl_status
    root, status = parse_root(root_arg)
    if status is not None or root is None:
        return status or 64
    baseline = Path(baseline_arg).absolute() if baseline_arg is not None else None
    metadata, status = run_metadata(root, "the cargo metadata shape is invalid for the checked members")
    if status is not None or metadata is None:
        return status or 2
    for member in metadata.members:
        if not member.publishable:
            continue
        if baseline is None:
            versions = index_versions(sparse_index(member.name))
            if not versions:
                print(f"skipping {member.name}: never published")
                continue
            if not any(version != "0.0.0" for version in versions):
                print(f"skipping {member.name}: only the 0.0.0 name reservation is published")
                continue
        command = ["cargo", "semver-checks", "-p", member.name]
        if baseline is not None:
            command.extend(["--baseline-root", str(baseline)])
        checked = subprocess.run(command, cwd=root, check=False)
        if checked.returncode:
            return error(
                f"semver violation in {member.name}; fix the change or bump the minor",
                1,
            )
    return 0


def main(arguments: list[str]) -> int:
    if not arguments:
        return usage_error("usage: scripts/release.py <publish|semver> ...")
    mode, *rest = arguments
    if mode == "publish":
        return publish_main(rest)
    if mode == "semver":
        return semver_main(rest)
    return usage_error("usage: scripts/release.py <publish|semver> ...")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
