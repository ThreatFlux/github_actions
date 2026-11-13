"""Replace only reviewed value spans inside exact maintained-field contexts."""

import re

from api import PolicyError, safe_path

VALUE = "@@RUST_VALUE@@"


def version_tuple(version):
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise PolicyError("Only explicit stable Rust versions are managed")
    return tuple(map(int, version.split(".")))


def render(version, style):
    version_tuple(version)
    if style == "plain":
        return version
    if style == "escaped":
        return version.replace(".", "\\.")
    if style == "bracketed":
        return version.replace(".", "[.]")
    raise PolicyError("Unknown compiler value format")


def states(policy):
    values = [*policy["accepted_states"], policy["target"]]
    versions = [state["stable"]["version"] for state in values]
    target = version_tuple(policy["target"]["stable"]["version"])
    if any(version_tuple(version) > target for version in versions):
        raise PolicyError("Policy would accept a newer compiler and downgrade it")
    return values


def possibilities(policy, kind):
    if kind.startswith("image:"):
        variant = kind.split(":", 1)[1]
        return {state["images"][variant]["reference"] for state in states(policy)}
    return {render(state["stable"]["version"], kind) for state in states(policy)}


def desired(policy, kind):
    if kind.startswith("image:"):
        return policy["target"]["images"][kind.split(":", 1)[1]]["reference"]
    return render(policy["target"]["stable"]["version"], kind)


def expression(template, policy, kind):
    if template.count(VALUE) != 1:
        raise PolicyError("Adapter must bind exactly one value span")
    tokens = {VALUE: "(?P<value>" + alternatives(possibilities(policy, kind)) + ")"}
    for style in ["plain", "escaped", "bracketed"]:
        tokens[f"@@RUST_{style}@@"] = alternatives(possibilities(policy, style))
    for variant in policy["target"]["images"]:
        tokens[f"@@IMAGE_{variant}@@"] = alternatives(
            possibilities(policy, "image:" + variant)
        )
    parts = re.split("(" + "|".join(map(re.escape, tokens)) + ")", template)
    return re.compile("".join(tokens.get(part, re.escape(part)) for part in parts))


def alternatives(values):
    return "(?:" + "|".join(re.escape(value) for value in sorted(values)) + ")"


def scoped(text, job):
    if job is None:
        return 0, len(text)
    jobs = list(re.finditer(r"(?m)^  ([A-Za-z0-9_-]+):[^\n]*$", text))
    selected = [match for match in jobs if match.group(1) == job]
    if len(selected) != 1:
        raise PolicyError("Reviewed workflow job context changed")
    begin = selected[0].start()
    end = next((match.start() for match in jobs if match.start() > begin), len(text))
    return begin, end


def patch_file(text, adapters, policy):
    replacements = {}
    for adapter in adapters:
        begin, end = scoped(text, adapter.get("job"))
        pattern = expression(adapter["template"], policy, adapter["kind"])
        matches = list(pattern.finditer(text, begin, end))
        if len(matches) != adapter["count"]:
            raise PolicyError(
                "Maintained Rust selector is missing, ambiguous, or changed"
            )
        for match in matches:
            span = match.span("value")
            replacement = desired(policy, adapter["kind"])
            if span in replacements and replacements[span] != replacement:
                raise PolicyError("Conflicting maintained Rust selector adapters")
            replacements[span] = replacement
    spans = sorted(replacements)
    if any(left[1] > right[0] for left, right in zip(spans, spans[1:])):
        raise PolicyError("Overlapping maintained Rust selector adapters")
    for begin, end in reversed(spans):
        text = text[:begin] + replacements[(begin, end)] + text[end:]
    return text


def validate_file(path):
    safe_path(path)
    forbidden = {
        "vendor",
        "fixtures",
        "fuzz",
        "compat",
        "historical",
        "legacy",
        "snapshots",
        "tests",
    }
    if forbidden.intersection(path.lower().split("/")) or path.endswith("Cargo.lock"):
        raise PolicyError(
            "Compatibility, vendor, fixture and lock inputs are not managed"
        )
    if path.endswith("Cargo.toml"):
        raise PolicyError("Cargo package rust-version is not a rollout selector")
