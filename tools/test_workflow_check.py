#!/usr/bin/env python3
"""tools/workflow_check.py: a failing workflow for each of its four rules,
the look-alikes each rule must let through, the lists' entries nothing
needs, the reader of the YAML the workflows are written in, and then the
repository's own workflows, which pass.

    python3 tools/test_workflow_check.py
"""

import contextlib
import importlib.util
import os
import pathlib
import subprocess
import sys
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("workflow_check", os.path.join(HERE, "workflow_check.py"))
wc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(wc)

PIN = "3d3c42e5aac5ba805825da76410c181273ba90b1"


def workflow(jobs, top="permissions:\n  contents: read\n"):
    """A workflow around `jobs` (YAML at the jobs' own indent), `@PIN` the
    commit every pinned action names."""
    body = textwrap.indent(textwrap.dedent(jobs).strip("\n"), "  ")
    return ("name: t\non: push\n" + top + "jobs:\n" + body + "\n").replace("@PIN", "@" + PIN)


def findings(text):
    return wc.check_text(text, "t.yml")


@contextlib.contextmanager
def lists(**given):
    """The check's lists as a test gives them (`WRITE_SCRIPTS={...}`), put
    back after."""
    saved = {name: dict(getattr(wc, name)) for name in given}
    try:
        for name, value in given.items():
            getattr(wc, name).clear()
            getattr(wc, name).update(value)
        yield
    finally:
        for name, value in saved.items():
            getattr(wc, name).clear()
            getattr(wc, name).update(value)


class Clean(unittest.TestCase):
    def test_a_workflow_keeping_every_rule_has_no_finding(self):
        text = workflow("""
            build:
              runs-on: ubuntu-latest
              steps:
                - uses: actions/checkout@PIN # v7.0.1
                  with:
                    persist-credentials: false
                - run: cargo build --release
                - uses: actions/upload-artifact@PIN # v7.0.2
                  with:
                    name: dist
                    path: target/release/x
            publish:
              needs: build
              runs-on: ubuntu-latest
              environment: release
              permissions:
                contents: write
              steps:
                - uses: actions/download-artifact@PIN # v8.0.2
                - run: gh release upload "$TAG" dist/*
                  env:
                    GH_TOKEN: ${{ github.token }}
                    KEY: ${{ secrets.KEY }}
        """)
        self.assertEqual(findings(text), [])


class Pins(unittest.TestCase):
    def one(self, uses, line=9):
        text = workflow(f"""
            a:
              runs-on: ubuntu-latest
              steps:
                - uses: {uses}
        """)
        got = findings(text)
        self.assertEqual(len(got), 1, got)
        self.assertTrue(got[0].startswith(f"t.yml:{line}: job `a` uses "), got)
        return got[0]

    def test_a_tag_is_refused(self):
        self.assertIn("40-hex commit", self.one("actions/checkout@v7"))

    def test_a_branch_is_refused(self):
        self.assertIn("40-hex commit", self.one("pypa/gh-action-pypi-publish@release/v1"))

    def test_a_short_commit_is_refused(self):
        self.assertIn("40-hex commit", self.one("actions/checkout@3d3c42e # v7.0.1"))

    def test_no_ref_at_all_is_refused(self):
        self.assertIn("40-hex commit", self.one("actions/checkout"))

    def test_a_commit_without_its_tag_is_refused(self):
        self.assertIn("names no tag", self.one(f"actions/checkout@{PIN}"))

    def test_a_comment_that_is_no_tag_is_refused(self):
        self.assertIn("names no tag", self.one(f"actions/checkout@{PIN} # the latest"))

    def test_the_comment_names_the_most_specific_tag(self):
        # Dependabot rewrites only a comment naming the commit's most
        # specific tag: a `# v7` would stay `# v7` over a pin moved to v8.
        for comment in ("v7", "v7.0", "7"):
            with self.subTest(comment=comment):
                got = self.one(f"actions/checkout@{PIN} # {comment}")
                self.assertIn(f"names `{comment}`, not the commit's most specific tag", got)
        for comment in ("v7.0.1", "7.0.1", "v1.0.0-rc.1", "v7.0.1 the tag CI ran"):
            with self.subTest(comment=comment):
                self.assertEqual(findings(workflow(f"""
                    a:
                      runs-on: ubuntu-latest
                      steps:
                        - uses: actions/checkout@PIN # {comment}
                """)), [])

    def test_an_action_publishing_no_full_tag_may_name_its_major(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - uses: some/action/sub@PIN # v2
                - uses: other/action@PIN # v2
                - uses: some/action@PIN # the latest
        """)
        with lists(MAJOR_TAGS={"some/action": "a test's: it publishes v2 and no v2.x.y"}):
            got = findings(text)
        self.assertEqual(len(got), 2, got)
        self.assertIn("uses other/action@", got[0])
        self.assertIn("names no tag", got[1])

    def test_an_action_in_a_subdirectory_is_held_the_same(self):
        self.assertIn("40-hex commit", self.one("gradle/actions/setup-gradle@v6"))
        self.assertEqual(findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - uses: gradle/actions/setup-gradle@PIN # v6.4.1
        """)), [])

    def test_a_docker_image_is_pinned_by_its_digest(self):
        self.assertIn("digest", self.one("docker://alpine:3.20"))
        digest = "sha256:" + "a" * 64
        self.assertEqual(findings(workflow(f"""
            a:
              runs-on: ubuntu-latest
              steps:
                - uses: docker://alpine@{digest}
        """)), [])

    def test_a_local_action_needs_no_pin(self):
        self.assertEqual(findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - uses: ./.github/actions/setup
        """)), [])

    def test_a_reusable_workflow_is_held_too(self):
        got = findings(workflow("""
            a:
              uses: org/repo/.github/workflows/build.yml@v1
        """))
        self.assertEqual(len(got), 1, got)
        self.assertIn("t.yml:7: job `a` uses org/repo/.github/workflows/build.yml@v1", got[0])

    def test_a_step_written_as_a_flow_mapping_is_read(self):
        got = findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - { name: out, uses: actions/checkout@v7 }
        """))
        self.assertEqual(len(got), 1, got)

    def test_a_uses_line_inside_a_script_is_none(self):
        self.assertEqual(findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - run: |
                    cat > action.yml <<'EOF'
                    runs:
                      steps:
                        - uses: actions/checkout@v7
                    EOF
                - name: >-
                    uses: actions/cache@v4
                  run: 'echo "uses: actions/cache@v4"'
        """)), [])


class Permissions(unittest.TestCase):
    JOB = """
        a:
          runs-on: ubuntu-latest
          permissions:
            contents: read
          steps:
            - run: echo hi
    """

    def test_a_workflow_without_permissions_at_its_top_is_refused(self):
        got = findings(workflow(self.JOB, top=""))
        self.assertEqual(len(got), 1, got)
        self.assertIn("t.yml:1: no `permissions:` at the top", got[0])

    def test_a_workflow_with_them_passes(self):
        self.assertEqual(findings(workflow(self.JOB)), [])
        self.assertEqual(findings(workflow(self.JOB, top="permissions: read-all\n")), [])
        self.assertEqual(findings(workflow(self.JOB, top="permissions: {}\n")), [])


class WritesAndBuilds(unittest.TestCase):
    def job(self, permissions, run=None, top="permissions:\n  contents: read\n", uses=None, shell=None):
        steps = []
        if uses:
            steps.append(f"- uses: {uses}")
        if run is not None:
            steps.append("- run: |\n" + textwrap.indent(textwrap.dedent(run).strip("\n"), "    ")
                         + (f"\n  shell: {shell}" if shell else ""))
        perms = f"permissions:\n{textwrap.indent(permissions, '  ')}\n" if permissions else ""
        jobs = "a:\n  runs-on: ubuntu-latest\n" + textwrap.indent(perms, "  ") + "  steps:\n" + \
            textwrap.indent("\n".join(steps), "    ")
        return findings(workflow(jobs, top=top))

    def assertOff(self, got, what):
        """One finding: job `a` holds a write and runs `what`."""
        self.assertEqual(len(got), 1, got)
        self.assertIn("job `a` holds", got[0])
        self.assertIn(f"write and runs {what}", got[0])
        self.assertIn("a job that writes runs only what the check lists", got[0])

    def test_the_workflows_write_reaches_a_job_naming_none(self):
        got = self.job(None, "cargo build --release", top="permissions:\n  contents: write\n")
        self.assertOff(got, "`cargo`, a program WRITE_COMMANDS does not list")
        self.assertIn("holds contents write", got[0])

    def test_a_jobs_own_write(self):
        self.assertOff(self.job("packages: write", "docker build -t x ."), "`docker build -t`, a use of docker")
        self.assertOff(self.job("packages: write", "docker buildx build --push -t x ."),
                       "`docker buildx build`, a use of docker")
        self.assertOff(self.job("contents: write", "./gradlew assemble"),
                       "`./gradlew`, a program or script by its path WRITE_SCRIPTS does not list")
        self.assertOff(self.job("contents: write", "make wasm"), "`make`")

    def test_write_all(self):
        got = self.job(None, "npm run build", top="permissions: write-all\n")
        self.assertOff(got, "`npm run build`, a use of npm WRITE_COMMANDS does not list")
        self.assertIn("holds every scope (write-all) write", got[0])

    def test_id_token_is_a_write(self):
        self.assertOff(self.job("id-token: write", "npm ci"), "`npm ci`")
        self.assertOff(self.job("id-token: write", "pnpm install --frozen-lockfile"), "`pnpm`")

    def test_a_command_where_a_shell_starts_one(self):
        for run, what in (("set -eu\nfor t in a b; do cargo build --target \"$t\"; done", "`cargo`"),
                          ("if true; then cargo test; fi", "`cargo`"),
                          ("RUSTFLAGS=-Dwarnings cargo build", "`cargo`"),
                          ("sudo env HOME=/root cargo build", "`cargo`"),
                          ("~/.cargo/bin/cargo build", "`~/.cargo/bin/cargo`"),
                          ("echo start && cargo build", "`cargo`"),
                          ("out=$(cargo metadata --format-version 1)", "`cargo`"),
                          ("ANDROID=1 \\\n  cargo build", "`cargo`")):
            with self.subTest(run=run):
                self.assertOff(self.job("contents: write", run), what)

    def test_each_build_beside_a_write(self):
        # None of these is named anywhere in the check: each is refused for
        # not being listed, the reviewer's misses of the old deny-list too.
        for run, what in (
                ("swift build -c release", "`swift`"),
                ("dotnet pack -c Release -o dist", "`dotnet pack -c`"),
                ("dotnet build", "`dotnet build`"),
                ("dotnet msbuild x.sln", "`dotnet msbuild x.sln`"),
                ("pip install .", "`pip`"),
                ("python3 -m pip install -e .", "`python3`"),
                ("python3 setup.py sdist", "`python3`"),
                ("go build ./...", "`go`"),
                ("go -C integrations/go build ./...", "`go`"),
                ("npx wrangler deploy", "`npx`"),
                ("xcodebuild -scheme NotesKit build -quiet", "`xcodebuild`"),
                ("xcrun xcodebuild -scheme NotesKit build", "`xcrun`"),
                ("flutter build apk --debug", "`flutter`"),
                ("dart compile exe bin/x.dart", "`dart`"),
                ("mvn -B package", "`mvn`"),
                ("npm run lint", "`npm run lint`"),
                ("npm --prefix web run build", "`npm --prefix web`"),
                ("npm -w @fennecdb/web ci", "`npm -w @fennecdb/web`"),
                ("node --run build", "`node --run build`"),
                ("node scripts/build.mjs", "`node scripts/build.mjs`"),
                ("bun run build", "`bun`"),
                ("yarn", "`yarn`"),
                ("pnpm lint", "`pnpm`"),
                ("corepack pnpm install", "`corepack`"),
                ("uv build", "`uv`"),
                ("cmake --build build --parallel", "`cmake`"),
                ("ninja -C build", "`ninja`"),
                ("msbuild FennecDb.sln /p:Configuration=Release", "`msbuild`"),
                ("docker compose up --build -d", "`docker compose up`"),
                ("docker --context remote build .", "`docker --context remote`"),
                ("integrations/packages.sh dist-packages", "`integrations/packages.sh`"),
                ("sh integrations/dotnet/package.sh dist-nuget", "`integrations/dotnet/package.sh`"),
                ("tools/linux-packages.sh 1.0.0 tarballs dist amd64", "`tools/linux-packages.sh`"),
                ("./pack.ps1", "`./pack.ps1`")):
            with self.subTest(run=run):
                self.assertOff(self.job("contents: write", run), what)

    def test_a_listed_program_put_to_another_use(self):
        for run, what in (
                ("gh extension install someone/gh-build", "`gh extension install`, a use of gh"),
                ("gh build", "`gh build`, a use of gh"),
                ("git -c alias.b='!cargo build' b", "`git -c alias.b=!cargo build`, a use of git"),
                ("git config core.hooksPath .githooks", "`git config core.hooksPath`: git's configuration"),
                ("git submodule update --init", "`git submodule update`, a use of git"),
                ("npm publish ./web --access public", "`npm publish` without `--ignore-scripts`"),
                ("docker run --rm image", "`docker run --rm`, a use of docker"),
                ("docker buildx bake", "`docker buildx bake`, a use of docker"),
                ("dotnet nuget push x.nupkg && dotnet run", "`dotnet run`, a use of dotnet"),
                ("tar -I zstd -xf x.tar.zst", "`tar -I`, an option that starts a program"),
                ("tar -xIf zstd x.tar.zst", "`tar -xIf`, an option that starts a program"),
                ("tar xIf zstd x.tar.zst", "`tar xIf`, an option that starts a program"),
                ("tar --to-command='sh -c x' -xf a.tar", "`tar --to-command=sh -c x`, an option"),
                ("tar --use-compress-program=zstd -xf a.tar", "`tar --use-compress-program=zstd`")):
            with self.subTest(run=run):
                self.assertOff(self.job("contents: write", run), what)

    def test_what_the_lists_hold_passes(self):
        run = """
            # cargo build ran in the job before; this one publishes
            echo "run cargo build first" # make
            set -eu
            docker load -i image.tar
            docker image inspect -f '{{.Os}}' "$IMAGE"
            docker push "$IMAGE"
            docker buildx imagetools create -t "$IMAGE:1" "$IMAGE:1-amd64"
            spec=$(tar -xzOf "$pkg" package/package.json | node -p 'JSON.parse(require("fs").readFileSync(0)).name')
            npm view "$spec" version
            npm publish "./pkg.tgz" --access public --provenance --ignore-scripts
            dotnet nuget push dist/x.nupkg --skip-duplicate
            git config user.name 'github-actions[bot]'
            git push origin "refs/tags/$tag"
            gh release upload "$tag" dist/* --clobber
            gh pr create --head "$branch" || echo "open it by hand"
            sha256sum *.tar.gz > SHA256SUMS
            CARGO_TERM_COLOR=always echo "$CARGO_TERM_COLOR"
            ls ~/.cargo/registry | grep -c . | tr -d ' '
            cat build.gradle.kts
            make_it=1 echo "${#make_it}"
            [ -n "$(jq -r .digest m.json)" ] || exit 1
            workflow/tools/image-latest.sh "$IMAGE" "$TAG"
            sh integrations/studio-desktop/scripts/attach.sh "$RELEASE" dist/*
            "$GITHUB_WORKSPACE/integrations/swift/set-binary.sh" 1.0.0 "$SUM"
        """
        self.assertEqual(self.job("contents: write\npackages: write\nid-token: write", run), [])

    def test_a_function_the_script_defines(self):
        # Its call is no program; its body is read where it stands.
        self.assertEqual(self.job("packages: write", """
            digest() { docker buildx imagetools inspect --format '{{json .Manifest}}' "$1" | jq -r .digest; }
            amd=$(digest "$IMAGE:1-amd64")
            function arm { digest "$IMAGE:1-arm64"; }
            arm
        """), [])
        self.assertOff(self.job("contents: write", "build() { cargo build; }\nbuild"), "`cargo`")

    def test_a_shell_runs_what_is_read_or_listed(self):
        self.assertEqual(self.job("contents: write", """
            sh -c 'gh release view "$1"' sh v1
            bash -eo pipefail -c 'gh release view v1 | jq .'
            sh <<'EOF'
            gh release view v1
            EOF
            sh tools/image-latest.sh "$IMAGE" "$TAG"
        """), [])
        for run, what in (
                ("curl -fsSL https://example.com/x.sh | sh", "`sh` reading its script from its input"),
                ("bash <<< 'gh release view v1'", "`bash` reading its script from its input"),
                ("bash < script.sh", "`bash` reading its script from its input"),
                ("sh tools/other.sh", "`tools/other.sh`, a program or script by its path"),
                (". ./env.sh", "`./env.sh`, a program or script by its path"),
                ("source tools/vars.sh", "`tools/vars.sh`"),
                ("bash -s v1 <<'EOF'\ncargo build\nEOF", "`cargo`"),
                ("pwsh -File pack.ps1", "`pack.ps1`")):
            with self.subTest(run=run):
                got = self.job("contents: write", run)
                self.assertTrue(any(f"write and runs {what}" in f for f in got), got)

    def test_a_listed_script_wherever_the_repository_is_checked_out(self):
        for run in ("workflow/tools/image-latest.sh a b", "./tools/image-latest.sh a b",
                    "\"$GITHUB_WORKSPACE\"/tools/image-latest.sh a b"):
            with self.subTest(run=run):
                self.assertEqual(self.job("packages: write", run), [])
        for run in ("tools/image-latest.sh.bak a b", "other-tools/image-latest.sh a b", "image-latest.sh a b"):
            with self.subTest(run=run):
                self.assertEqual(len(self.job("packages: write", run)), 1)

    def test_an_action_an_image_a_workflow_and_a_shell_off_the_lists(self):
        self.assertOff(self.job("packages: write", "echo hi", uses=f"docker/build-push-action@{PIN} # v6.18.0"),
                       "the action `docker/build-push-action`, which WRITE_ACTIONS does not list")
        self.assertOff(self.job("contents: write", "echo hi", uses=f"gradle/actions/setup-gradle@{PIN} # v6.4.1"),
                       "the action `gradle/actions/setup-gradle`")
        self.assertOff(self.job("contents: write", "echo hi", uses="docker://alpine@sha256:" + "a" * 64),
                       "the image `docker://alpine@sha256:")
        self.assertOff(self.job("contents: write", "print('hi')", shell="python"),
                       "a script under `shell: python`, which this check does not read")
        # A listed action, written in another case, is that action.
        self.assertEqual(self.job("id-token: write", None, uses=f"nuget/LOGIN@{PIN} # v1.2.0"), [])
        got = findings(workflow("""
            a:
              permissions:
                contents: write
              uses: org/repo/.github/workflows/build.yml@PIN # v1.0.0
        """))
        self.assertEqual(len(got), 1, got)
        self.assertIn("runs the workflow `org/repo/.github/workflows/build.yml@", got[0])

    def test_a_refusal_names_the_list_an_entry_would_go_in(self):
        # What no list holds may be listed, if it builds nothing: the
        # finding names the list. What no entry would let by names none.
        for run, uses, listed in (
                ("cargo build", None, "WRITE_COMMANDS"),
                ("npm run build", None, "WRITE_COMMANDS"),
                ("./gradlew assemble", None, "WRITE_SCRIPTS"),
                ("echo hi", f"docker/build-push-action@{PIN} # v6.18.0", "WRITE_ACTIONS"),
                ("npm publish ./web", None, None),
                ("tar -I zstd -xf x.tar.zst", None, None),
                ("echo hi", "./.github/actions/setup", None)):
            with self.subTest(run=run, uses=uses):
                got = self.job("contents: write", run, uses=uses)
                self.assertEqual(len(got), 1, got)
                self.assertIn("build in a job that reads alone and hand what it made to this one as an"
                              " artifact", got[0])
                if listed:
                    self.assertTrue(got[0].endswith(f"as an artifact, or list it in {listed} with its reason,"
                                                    " if it builds nothing"), got[0])
                else:
                    self.assertNotIn("or list it in", got[0])

    def test_a_local_action_beside_a_write(self):
        # Its steps are another file's, which this check does not read.
        uses = "./.github/actions/setup"
        self.assertOff(self.job("contents: write", "echo hi", uses=uses),
                       "the local action `./.github/actions/setup`, whose steps this check does not read")
        self.assertEqual(self.job("contents: read", "echo hi", uses=uses), [])

    def test_a_job_that_reads_may_build(self):
        self.assertEqual(self.job("contents: read", "cargo build\nmake wasm\ndocker build ."), [])
        self.assertEqual(self.job(None, "cargo build"), [])

    def test_a_job_narrowing_a_workflows_write_may_build(self):
        self.assertEqual(self.job("contents: read", "cargo build", top="permissions:\n  contents: write\n"), [])

    def test_text_a_command_is_handed_is_no_command(self):
        run = """
            echo "a; cargo build" && echo 'x && make y'
            printf '%s\\n' "npm ci" | grep -c npm
            git commit -m "make it so" -m "then swift build"
            echo "::notice::run npx wrangler, then go build"
            cat > action.yml <<'EOF'
            cargo build
            $(make wasm)
            EOF
            case $state in
              make) echo "a pattern, not a command" ;;
              cargo | npx) exit 0 ;;
            esac
            command -v cargo >/dev/null || echo "no cargo"
            for tool in cargo make npx; do echo "$tool"; done
            npm publish "./pkg.tgz" --ignore-scripts && dotnet nuget push x.nupkg
            x=$((1 + 2)) # make
            [[ -n $x && $x < 3 || ( -z $y ) ]] && echo small
            (( x += 1 ))
            trap 'echo done' EXIT
        """
        self.assertEqual(self.job("contents: write", run), [])

    def test_what_runs_inside_a_word_or_a_shell_is_read(self):
        for run, what in (
                ('echo "$(cargo build)"', "`cargo`"),
                ('echo "`make`"', "`make`"),
                ("bash -c 'cargo build --release'", "`cargo`"),
                ('sh -ec "make wasm"', "`make`"),
                ('eval "go build ./..."', "`go`"),
                ("cat <(cargo tree)", "`cargo`"),
                ("sh <<'EOF'\ncargo build\nEOF", "`cargo`"),
                ("bash -s v1 <<'EOF'\ncargo build\nEOF", "`cargo`"),
                ("bash -es -- v1 <<'EOF'\ncargo build\nEOF", "`cargo`"),
                ("cat > out <<EOF\n$(swift build)\nEOF", "`swift`"),
                ('echo "${TAG:-$(cargo build)}"', "`cargo`"),
                (': "${OUT:=`make`}"', "`make`"),
                ('echo "${A:-${B:-$(cargo build)}}"', "`cargo`"),
                ("echo $(( $(cargo metadata) + 1 ))", "`cargo`"),
                ("(( n += $(cargo metadata) ))", "`cargo`"),
                ("[[ -n $(cargo metadata) ]]", "`cargo`"),
                ("trap 'cargo build' EXIT", "`cargo`"),
                ("env -S 'cargo build' x", "`cargo`"),
                ("xargs -I {} dotnet pack {}", "`dotnet pack {}`"),
                ("sudo -u builder env CC=clang make", "`make`")):
            with self.subTest(run=run):
                self.assertOff(self.job("contents: write", run), what)

    def test_a_script_handed_too_deep_is_a_finding(self):
        self.assertEqual(self.job("contents: write", "eval " * 8 + "echo hi"), [])
        got = self.job("contents: write", "eval " * 9 + "echo hi")
        self.assertOff(got, f"`a script handed to a shell {wc.DEPTH} deep`")

    def test_powershell_is_read_as_powershell(self):
        def names(text):
            commands, _ = wc.scan(textwrap.dedent(text), pwsh=True)
            return [wc.name_of(words[0]) for words, _ in commands]
        self.assertEqual(names("""
            Write-Host "cargo build"; Write-Host `"make`"
            <# make
               npx #> Write-Output "x"
            $s = @'
            cargo build
            '@
        """), ["write-host", "write-host", "write-output", "$s"])
        self.assertIn("cargo", names("Get-Item x | ForEach-Object { cargo build }"))
        # A Windows runner's own shell is PowerShell: its backquote escapes.
        def job(run):
            return findings(workflow(
                "a:\n  runs-on: windows-latest\n  permissions:\n    contents: write\n  steps:\n"
                f"    - run: {run}\n"))
        got = job("Write-Host '`\"make`\"'")
        self.assertEqual(len(got), 1, got)
        self.assertIn("runs `write-host`", got[0])
        self.assertIn("runs `cargo`", job("cargo build")[0])

    def test_a_job_let_off_by_name(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              permissions:
                contents: write
              steps:
                - run: cargo build
        """)
        with lists(WRITE_AND_BUILD={("t.yml", "a"): "a test's"}):
            self.assertEqual(findings(text), [])
        with lists(WRITE_AND_BUILD={("t.yml", "b"): "another job's"}):
            got = findings(text)
        self.assertEqual(len(got), 2, got)
        self.assertIn("job `a` holds contents write and runs `cargo`", got[0])
        self.assertIn("WRITE_AND_BUILD lets off job `b`, which this workflow does not have", got[1])

    def test_a_job_let_off_that_runs_only_what_is_listed_is_a_finding(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              permissions:
                contents: write
              steps:
                - run: gh release upload v1 dist/*
        """)
        with lists(WRITE_AND_BUILD={("t.yml", "a"): "a test's"}):
            got = findings(text)
        self.assertEqual(len(got), 1, got)
        self.assertIn("WRITE_AND_BUILD lets it off, and it runs nothing beside a write that the lists"
                      " do not hold: take it out", got[0])


class Secrets(unittest.TestCase):
    def test_a_secret_in_a_job_without_an_environment(self):
        got = findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - run: deploy
                  env:
                    TOKEN: ${{ secrets.DEPLOY_TOKEN }}
        """))
        self.assertEqual(len(got), 1, got)
        self.assertIn("t.yml:11: job `a` reads secrets.DEPLOY_TOKEN and names no `environment:`", got[0])

    def test_every_form_of_reading_one(self):
        for expr in ("${{ secrets['DEPLOY_TOKEN'] }}", "${{ toJSON(secrets) }}",
                     "${{ secrets.A || secrets.B }}", "x${{secrets.A}}y"):
            with self.subTest(expr=expr):
                got = findings(workflow(f"""
                    a:
                      runs-on: ubuntu-latest
                      steps:
                        - uses: some/action@PIN # v1.0.0
                          with:
                            token: "{expr}"
                """))
                self.assertTrue(got and all("names no `environment:`" in f for f in got), got)

    def test_secrets_inherit(self):
        got = findings(workflow("""
            a:
              uses: ./.github/workflows/publish.yml
              secrets: inherit
        """))
        self.assertEqual(len(got), 1, got)
        self.assertIn("every secret (`secrets: inherit`)", got[0])

    def test_a_job_naming_its_environment_may(self):
        for environment in ("environment: pypi", "environment:\n  name: pypi\n  url: https://pypi.org"):
            with self.subTest(environment=environment):
                jobs = ("a:\n  runs-on: ubuntu-latest\n" + textwrap.indent(environment, "  ") + "\n"
                        "  steps:\n"
                        "    - run: publish\n"
                        "      env:\n"
                        "        TOKEN: ${{ secrets.PYPI_API_TOKEN }}\n")
                self.assertEqual(findings(workflow(jobs)), [])

    def test_the_jobs_own_token_is_no_stored_secret(self):
        self.assertEqual(findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - run: gh release view
                  env:
                    GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
                    ALSO: ${{ secrets.github_token }}
        """)), [])

    def test_pythons_secrets_and_a_property_of_that_name_are_none(self):
        self.assertEqual(findings(workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - run: python3 -c 'import secrets, sys; sys.stdout.write(secrets.token_urlsafe(32))'
                - run: echo "${{ inputs.secrets }} ${{ env.secrets_dir }}"
                  env:
                    NOTE: secrets.NOT_AN_EXPRESSION
        """)), [])

    def test_a_secret_at_the_top_level_reaches_every_job(self):
        got = findings(workflow("""
            a:
              runs-on: ubuntu-latest
              environment: deploy
              steps:
                - run: deploy
        """, top="permissions:\n  contents: read\nenv:\n  TOKEN: ${{ secrets.DEPLOY_TOKEN }}\n"))
        self.assertEqual(len(got), 1, got)
        self.assertIn("the workflow's `env:` reads secrets.DEPLOY_TOKEN", got[0])

    def test_a_job_let_off_by_name(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              steps:
                - run: deploy
                  env:
                    TOKEN: ${{ secrets.DEPLOY_TOKEN }}
        """)
        with lists(REPOSITORY_SECRETS={("t.yml", "a"): "a test's"}):
            self.assertEqual(findings(text), [])
        with lists(REPOSITORY_SECRETS={("t.yml", "b"): "another job's"}):
            got = findings(text)
        self.assertEqual(len(got), 2, got)
        self.assertIn("job `a` reads secrets.DEPLOY_TOKEN and names no `environment:`", got[0])
        self.assertIn("REPOSITORY_SECRETS lets off job `b`, which this workflow does not have", got[1])

    def test_a_job_let_off_that_names_its_environment_is_a_finding(self):
        # site.yml's `deploy` once its environment exists: the line naming
        # it returns, and the entry letting it off goes.
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              environment: site
              steps:
                - run: deploy
                  env:
                    TOKEN: ${{ secrets.DEPLOY_TOKEN }}
        """)
        with lists(REPOSITORY_SECRETS={("t.yml", "a"): "a test's"}):
            got = findings(text)
        self.assertEqual(len(got), 1, got)
        self.assertIn("REPOSITORY_SECRETS lets it off, and it reads no stored secret outside an"
                      " environment: take it out", got[0])


class Stale(unittest.TestCase):
    """An entry of a list that nothing among every workflow needs: a
    finding where every workflow is read."""

    CLEAN = workflow("""
        a:
          runs-on: ubuntu-latest
          steps:
            - uses: actions/checkout@PIN # v7.0.1
    """)

    def only(self, **given):
        """The lists a test gives, every other one empty: the repository's
        own entries name workflows a test does not hold."""
        empty = {name: {} for name in ("REPOSITORY_SECRETS", "WRITE_AND_BUILD", "MAJOR_TAGS", "WRITE_SCRIPTS")}
        return lists(**{**empty, **given})

    def test_a_job_let_off_in_a_workflow_that_is_gone(self):
        with self.only(WRITE_AND_BUILD={("gone.yml", "a"): "a test's"},
                       REPOSITORY_SECRETS={("renamed.yml", "deploy"): "a test's"}):
            got = wc.check_texts([("t.yml", self.CLEAN)])
            alone = wc.check_texts([("t.yml", self.CLEAN)], whole=False)
        self.assertEqual(len(got), 2, got)
        self.assertIn("REPOSITORY_SECRETS lets off job `deploy` of renamed.yml, which is no workflow here"
                      " (gone, or renamed): take it out", got[0])
        self.assertIn("WRITE_AND_BUILD lets off job `a` of gone.yml, which is no workflow here", got[1])
        # One workflow read alone says nothing of the others.
        self.assertEqual(alone, [])

    def test_an_action_let_off_its_major_that_no_pin_needs(self):
        def pinned(comment):
            return workflow(f"""
                a:
                  runs-on: ubuntu-latest
                  steps:
                    - uses: Some/Action@PIN # {comment}
            """)
        with self.only(MAJOR_TAGS={"some/action": "a test's: it publishes v2 and no v2.x.y"}):
            self.assertEqual(wc.check_texts([("t.yml", pinned("v2"))]), [])
            for text in (pinned("v2.1.0"), self.CLEAN):
                got = wc.check_texts([("t.yml", text)])
                self.assertEqual(len(got), 1, got)
                self.assertIn("MAJOR_TAGS lets off some/action, and no pin of it names a major tag alone:"
                              " take it out", got[0])

    def test_a_listed_script_no_job_that_writes_runs(self):
        def job(permissions):
            return workflow(f"""
                a:
                  runs-on: ubuntu-latest
                  permissions:
                    contents: {permissions}
                  steps:
                    - run: tools/x.sh
            """)
        with self.only(WRITE_SCRIPTS={"tools/x.sh": "a test's: it runs nothing at all"}):
            self.assertEqual(wc.check_texts([("t.yml", job("write"))]), [])
            got = wc.check_texts([("t.yml", job("read"))])
        self.assertEqual(len(got), 1, got)
        self.assertIn("WRITE_SCRIPTS lists tools/x.sh, which no job that writes runs: take it out", got[0])

    def test_the_lists_are_held_where_every_workflow_is_read(self):
        # One file named on the command line: none of the lists' entries is
        # stale for not being needed there.
        one = subprocess.run([sys.executable, os.path.join(HERE, "workflow_check.py"),
                              os.path.join(HERE, "..", ".github", "workflows", "ci.yml")],
                             capture_output=True, text=True, timeout=120)
        self.assertEqual(one.returncode, 0, one.stderr)
        self.assertIn("workflow_check: 1 workflows", one.stdout)


class Reader(unittest.TestCase):
    """The YAML the rules stand on, read as YAML reads it (every workflow of
    the repository was held to Ruby's Psych when the reader was written)."""

    def test_the_forms_workflows_take(self):
        doc = wc.read(textwrap.dedent("""\
            # a comment
            name: "ci: \\"all\\" \\u00e9"
            'on':
              push:
                branches: [main, 'release/*']
            jobs:
              a:
                strategy:
                  matrix:
                    include:
                    - { os: ubuntu-24.04, arch: amd64 }
                    - {os: windows-11-arm, target: "x:y"}
                steps:
                - uses: actions/checkout@abc # v7.0.1
                  with:
                    ref: ${{ github.event.inputs.tag || github.ref }}
                - run: |
                    echo one  # kept: a shell's comment

                    # kept too
                      indented
                  shell: bash
                - if: ${{ !cancelled() && github.event_name == 'push'
                    && github.ref == 'refs/heads/main' }}
                  run: >-
                    folded
                    text
                -
                  name: an entry on the line after its dash
                - 'it''s'
            """))
        self.assertEqual(doc["name"], 'ci: "all" \u00e9')
        self.assertEqual(doc["on"]["push"]["branches"], ["main", "release/*"])
        include = doc["jobs"]["a"]["strategy"]["matrix"]["include"]
        self.assertEqual(include, [{"os": "ubuntu-24.04", "arch": "amd64"},
                                   {"os": "windows-11-arm", "target": "x:y"}])
        steps = doc["jobs"]["a"]["steps"]
        self.assertEqual(steps[0]["uses"], "actions/checkout@abc")
        self.assertEqual(steps[0]["uses"].comment, "v7.0.1")
        self.assertEqual(steps[0]["uses"].line, 14)
        self.assertEqual(steps[0]["with"]["ref"], "${{ github.event.inputs.tag || github.ref }}")
        self.assertEqual(steps[1]["run"], "echo one  # kept: a shell's comment\n\n# kept too\n  indented")
        self.assertEqual(steps[1]["shell"], "bash")
        self.assertEqual(steps[2]["if"], "${{ !cancelled() && github.event_name == 'push' "
                                         "&& github.ref == 'refs/heads/main' }}")
        self.assertEqual(steps[2]["run"], "folded text")
        self.assertEqual(steps[3], {"name": "an entry on the line after its dash"})
        self.assertEqual(steps[4], "it's")

    def assertUnreadable(self, text, why):
        got = findings(text)
        self.assertEqual(len(got), 1, got)
        self.assertIn("cannot read this, so it passes nothing", got[0])
        self.assertIn(why, got[0])

    def test_what_it_does_not_read_is_a_finding(self):
        self.assertUnreadable("on: push\npermissions: {}\njobs:\n  a: &job\n    runs-on: x\n  b: *job\n",
                              "anchor, alias or tag")
        self.assertUnreadable("on: push\njobs:\n\ta:\n    runs-on: x\n", "a tab in the indentation")
        self.assertUnreadable("on: push\non: pull_request\n", "'on' twice")
        self.assertUnreadable("on: push\njobs:\n  a:\n    steps:\n      - run: x\n     bad: y\n",
                              "indented deeper than the mapping")
        self.assertUnreadable("on: [push\n", "does not close")


class Repository(unittest.TestCase):
    def test_every_entry_of_every_list_says_why(self):
        reasons = [(key, reason) for listed in (wc.REPOSITORY_SECRETS, wc.WRITE_AND_BUILD, wc.MAJOR_TAGS,
                                                wc.WRITE_SCRIPTS, wc.WRITE_ACTIONS)
                   for key, reason in listed.items()]
        reasons += [(name, reason) for name, (reason, _) in wc.WRITE_COMMANDS.items()]
        for key, reason in reasons:
            with self.subTest(key=key):
                self.assertIsInstance(reason, str)
                self.assertGreater(len(reason.split()), 4, f"{key}: say why it is let off or listed")

    def test_every_listed_script_is_the_repositorys(self):
        for script in wc.WRITE_SCRIPTS:
            with self.subTest(script=script):
                self.assertTrue((pathlib.Path(HERE).parent / script).is_file())

    def test_the_workflows_here_pass(self):
        root = pathlib.Path(HERE).parent / ".github" / "workflows"
        files = sorted(p for p in root.iterdir() if p.suffix in (".yml", ".yaml"))
        self.assertTrue(files)
        for path in files:
            with self.subTest(workflow=path.name):
                self.assertEqual(wc.check_text(path.read_text(encoding="utf-8"), path.name), [])
        # And every list's entry is one they need.
        self.assertEqual(wc.check_texts([(p.name, p.read_text(encoding="utf-8")) for p in files]), [])


if __name__ == "__main__":
    unittest.main()
