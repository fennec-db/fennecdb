#!/usr/bin/env python3
"""tools/workflow_check.py: a failing workflow for each of its four rules,
the look-alikes each rule must let through, the reader of the YAML the
workflows are written in, and then the repository's own workflows, which
pass.

    python3 tools/test_workflow_check.py
"""

import importlib.util
import os
import pathlib
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
        saved = dict(wc.MAJOR_TAGS)
        try:
            wc.MAJOR_TAGS["some/action"] = "a test's: it publishes v2 and no v2.x.y"
            got = findings(text)
        finally:
            wc.MAJOR_TAGS.clear()
            wc.MAJOR_TAGS.update(saved)
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
    def job(self, permissions, run=None, top="permissions:\n  contents: read\n", uses=None):
        steps = []
        if uses:
            steps.append(f"- uses: {uses}")
        if run is not None:
            steps.append("- run: |\n" + textwrap.indent(textwrap.dedent(run).strip("\n"), "    "))
        perms = f"permissions:\n{textwrap.indent(permissions, '  ')}\n" if permissions else ""
        jobs = "a:\n  runs-on: ubuntu-latest\n" + textwrap.indent(perms, "  ") + "  steps:\n" + \
            textwrap.indent("\n".join(steps), "    ")
        return findings(workflow(jobs, top=top))

    def assertBuild(self, got, what):
        self.assertEqual(len(got), 1, got)
        self.assertIn(what, got[0])
        self.assertIn("job `a` holds", got[0])

    def test_the_workflows_write_reaches_a_job_naming_none(self):
        got = self.job(None, "cargo build --release", top="permissions:\n  contents: write\n")
        self.assertBuild(got, "holds contents write and runs cargo")

    def test_a_jobs_own_write(self):
        self.assertBuild(self.job("packages: write", "docker build -t x ."), "runs docker build")
        self.assertBuild(self.job("packages: write", "docker buildx build --push -t x ."), "runs docker build")
        self.assertBuild(self.job("contents: write", "./gradlew assemble"), "runs gradle")
        self.assertBuild(self.job("contents: write", "make wasm"), "runs make")

    def test_write_all(self):
        self.assertBuild(self.job(None, "npm run build", top="permissions: write-all\n"), "write-all")

    def test_id_token_is_a_write(self):
        self.assertBuild(self.job("id-token: write", "npm ci"), "an npm, pnpm or yarn install, script or build")
        self.assertBuild(self.job("id-token: write", "pnpm install --frozen-lockfile"), "install, script or build")

    def test_a_command_where_a_shell_starts_one(self):
        for run in ("set -eu\nfor t in a b; do cargo build --target \"$t\"; done",
                    "if true; then cargo test; fi",
                    "RUSTFLAGS=-Dwarnings cargo build",
                    "sudo env HOME=/root cargo build",
                    "~/.cargo/bin/cargo build",
                    "echo start && cargo build",
                    "out=$(cargo metadata --format-version 1)",
                    "ANDROID=1 \\\n  cargo build"):
            with self.subTest(run=run):
                self.assertBuild(self.job("contents: write", run), "runs cargo")

    def test_gradle_set_up_in_a_job_that_writes(self):
        self.assertBuild(self.job("contents: write", "echo hi", uses=f"gradle/actions/setup-gradle@{PIN} # v6.4.1"),
                         "gradle/actions/setup-gradle")

    def test_a_script_named_for_building(self):
        self.assertBuild(self.job("contents: write", "integrations/kotlin/build-aar.sh dist"), "a build or package script")
        self.assertBuild(self.job("contents: write", "python3 site/build.py"), "a build or package script")

    def test_a_job_that_reads_may_build(self):
        self.assertEqual(self.job("contents: read", "cargo build\nmake wasm\ndocker build ."), [])
        self.assertEqual(self.job(None, "cargo build"), [])

    def test_a_job_narrowing_a_workflows_write_may_build(self):
        self.assertEqual(self.job("contents: read", "cargo build", top="permissions:\n  contents: write\n"), [])

    def test_what_is_no_build_passes(self):
        run = """
            # cargo build ran in the job before; this one publishes
            echo "run cargo build first" # make
            docker load -i image.tar
            docker push "$IMAGE"
            docker buildx imagetools create -t "$IMAGE:1" "$IMAGE:1-amd64"
            npm view "$spec" version
            npm publish "./pkg.tgz" --access public --provenance
            git push origin "refs/tags/$tag"
            gh release upload "$tag" dist/* --clobber
            sha256sum *.tar.gz > SHA256SUMS
            CARGO_TERM_COLOR=always printenv CARGO_TERM_COLOR
            ls ~/.cargo/registry
            cat build.gradle.kts
            make_it=1 echo "${#make_it}"
        """
        self.assertEqual(self.job("contents: write\npackages: write\nid-token: write", run), [])

    def test_each_build_beside_a_write(self):
        for run, what in (
                ("swift build -c release", "swift build"),
                ("swift test", "swift test"),
                ("dotnet pack -c Release -o dist", "dotnet pack"),
                ("dotnet build", "dotnet build"),
                ("pip install .", "pip install"),
                ("python3 -m pip install -e .", "pip install"),
                ("python3 setup.py sdist", "setup.py"),
                ("go build ./...", "go build"),
                ("go run github.com/magefile/mage@v1.17.2 -v build:linux", "go run"),
                ("npx wrangler deploy", "npx"),
                ("xcodebuild -scheme NotesKit build -quiet", "xcodebuild"),
                ("flutter build apk --debug", "flutter build"),
                ("dart compile exe bin/x.dart", "dart compile"),
                ("mvn -B package", "maven"),
                ("npm run lint", "an npm, pnpm or yarn install, script or build"),
                ("yarn", "an npm, pnpm or yarn install, script or build"),
                ("pnpm lint", "an npm, pnpm or yarn install, script or build"),
                ("docker compose up --build -d", "docker build"),
                ("docker --context remote build .", "docker build"),
                ("integrations/packages.sh dist-packages", "a build or package script"),
                ("sh integrations/dotnet/package.sh dist-nuget", "a build or package script"),
                ("tools/linux-packages.sh 1.0.0 tarballs dist amd64", "a build or package script"),
                ("./pack.ps1", "a build or package script")):
            with self.subTest(run=run):
                self.assertBuild(self.job("contents: write", run), f"runs {what}")

    def test_a_local_action_beside_a_write(self):
        # Its steps are another file's, which this check does not read.
        uses = "./.github/actions/setup"
        self.assertBuild(self.job("contents: write", "echo hi", uses=uses),
                         "runs the local action ./.github/actions/setup")
        self.assertEqual(self.job("contents: read", "echo hi", uses=uses), [])

    def test_text_a_command_is_handed_is_no_command(self):
        run = """
            echo "a; cargo build" && echo 'x && make y'
            printf '%s\\n' "npm ci" | tee note
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
            npm publish "./pkg.tgz" && dotnet nuget push x.nupkg && flutter pub publish --force
            yarn --version
            x=$((1 + 2)) # make
        """
        self.assertEqual(self.job("contents: write", run), [])

    def test_what_runs_inside_a_word_or_a_shell_is_read(self):
        for run, what in (
                ('echo "$(cargo build)"', "cargo"),
                ('echo "`make`"', "make"),
                ("bash -c 'cargo build --release'", "cargo"),
                ('sh -ec "make wasm"', "make"),
                ('eval "go build ./..."', "go build"),
                ("diff <(cargo tree) expected", "cargo"),
                ("sh <<'EOF'\ncargo build\nEOF", "cargo"),
                ("cat > out <<EOF\n$(swift build)\nEOF", "swift build"),
                ("xargs -I {} dotnet pack {}", "dotnet pack"),
                ("sudo -u builder env CC=clang make", "make")):
            with self.subTest(run=run):
                self.assertBuild(self.job("contents: write", run), f"runs {what}")

    def test_powershell_is_read_as_powershell(self):
        def job(run, shell="  shell: pwsh\n", runs_on="ubuntu-latest"):
            body = textwrap.indent(textwrap.dedent(run).strip("\n"), "        ")
            return findings(workflow(
                f"a:\n  runs-on: {runs_on}\n  permissions:\n    contents: write\n  steps:\n"
                f"    - run: |\n{body}\n" + textwrap.indent(shell, "    ")))
        self.assertEqual(job("""
            Write-Host "cargo build"; Write-Host `"make`"
            $s = @'
            cargo build
            '@
            <# make
               npx #> Write-Output $s
        """), [])
        self.assertEqual(len(job("Get-Item x | ForEach-Object { cargo build }")), 1)
        # A Windows runner's own shell is PowerShell: its backquote escapes.
        self.assertEqual(job('Write-Host "`"make`""', shell="", runs_on="windows-latest"), [])
        self.assertEqual(len(job("cargo build", shell="", runs_on="windows-latest")), 1)

    def test_a_job_let_off_by_name(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              permissions:
                contents: write
              steps:
                - run: cargo build
        """)
        saved = dict(wc.WRITE_AND_BUILD)
        try:
            wc.WRITE_AND_BUILD[("t.yml", "a")] = "a test's"
            self.assertEqual(findings(text), [])
            wc.WRITE_AND_BUILD.clear()
            wc.WRITE_AND_BUILD[("t.yml", "b")] = "another job's"
            got = findings(text)
            self.assertEqual(len(got), 2, got)
            self.assertIn("job `a` holds contents write and runs cargo", got[0])
            self.assertIn("WRITE_AND_BUILD lets off job `b`, which this workflow does not have", got[1])
        finally:
            wc.WRITE_AND_BUILD.clear()
            wc.WRITE_AND_BUILD.update(saved)

    def test_a_job_let_off_that_no_longer_builds_is_a_finding(self):
        text = workflow("""
            a:
              runs-on: ubuntu-latest
              permissions:
                contents: write
              steps:
                - run: gh release upload v1 dist/*
        """)
        saved = dict(wc.WRITE_AND_BUILD)
        try:
            wc.WRITE_AND_BUILD[("t.yml", "a")] = "a test's"
            got = findings(text)
        finally:
            wc.WRITE_AND_BUILD.clear()
            wc.WRITE_AND_BUILD.update(saved)
        self.assertEqual(len(got), 1, got)
        self.assertIn("WRITE_AND_BUILD lets it off, and it runs no build beside a write: take it out", got[0])


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
        saved = dict(wc.REPOSITORY_SECRETS)
        try:
            wc.REPOSITORY_SECRETS[("t.yml", "a")] = "a test's"
            self.assertEqual(findings(text), [])
            wc.REPOSITORY_SECRETS.clear()
            wc.REPOSITORY_SECRETS[("t.yml", "b")] = "another job's"
            got = findings(text)
            self.assertEqual(len(got), 2, got)
            self.assertIn("job `a` reads secrets.DEPLOY_TOKEN and names no `environment:`", got[0])
            self.assertIn("REPOSITORY_SECRETS lets off job `b`, which this workflow does not have", got[1])
        finally:
            wc.REPOSITORY_SECRETS.clear()
            wc.REPOSITORY_SECRETS.update(saved)

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
        saved = dict(wc.REPOSITORY_SECRETS)
        try:
            wc.REPOSITORY_SECRETS[("t.yml", "a")] = "a test's"
            got = findings(text)
        finally:
            wc.REPOSITORY_SECRETS.clear()
            wc.REPOSITORY_SECRETS.update(saved)
        self.assertEqual(len(got), 1, got)
        self.assertIn("REPOSITORY_SECRETS lets it off, and it reads no stored secret outside an"
                      " environment: take it out", got[0])


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
    def test_every_job_or_action_let_off_says_why(self):
        for listed in (wc.REPOSITORY_SECRETS, wc.WRITE_AND_BUILD, wc.MAJOR_TAGS):
            for key, reason in listed.items():
                with self.subTest(key=key):
                    self.assertIsInstance(reason, str)
                    self.assertGreater(len(reason.split()), 4, f"{key}: say why it is let off")

    def test_the_workflows_here_pass(self):
        root = pathlib.Path(HERE).parent / ".github" / "workflows"
        files = sorted(p for p in root.iterdir() if p.suffix in (".yml", ".yaml"))
        self.assertTrue(files)
        for path in files:
            with self.subTest(workflow=path.name):
                self.assertEqual(wc.check_text(path.read_text(encoding="utf-8"), path.name), [])


if __name__ == "__main__":
    unittest.main()
