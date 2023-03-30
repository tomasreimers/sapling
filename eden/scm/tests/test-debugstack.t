#debugruntest-compatible

  $ configure modern

Test utils:

  $ cat > pprint.py << 'EOS'
  > import json, pprint, sys
  > obj = json.load(sys.stdin)
  > s = pprint.pformat(obj, width=200)  # pformat is more compact than json
  > sys.stdout.buffer.write((s + "\n").encode())
  > EOS
  $ pprint() {
  >   python ~/pprint.py
  > }

  @command
  def marks(args, stdin, stdout, fs, marks={}):
      """Maintains 'marks'. Can be used to get or set marks->hashes.

      Use 'marks :1 :2' to convert marks to hex hashes in JSON.
      Use 'hg debugimportstack ... | marks' to track marks outputted from hg.
      Use 'hg ... | marks' to convert hex commit hashes back to marks.
      """
      import json
      input_bytes = stdin.read()
      if input_bytes:
          if input_bytes.startswith(b"{"):
              obj = json.loads(input_bytes.decode().splitlines()[0])
              marks.update(obj)
          else:
              for m, n in marks.items():
                  input_bytes = input_bytes.replace(n.encode(), m.encode())
          stdout.write(input_bytes)
      if args:
          stdout.write(json.dumps([marks[mark] for mark in args]).encode())

Export a linear stack of various kinds of files: modified, renamed, deleted,
non-utf8, symlink, executable:

  $ newrepo
  $ drawdag << 'EOS'
  > A..D
  > python:
  > commit('A', remotename='remote/master', files={"A":"1"})
  > commit('B', files={"A":"2", "B":"3 (executable)"})
  > commit('C', files={"C":b85(b"\xfbm"), "Z": "B (symlink)"})  # C: invalid utf-8
  > commit('D', files={"D":"2 (renamed from A)", "E": "E (copied from C)"})
  > EOS

Test that various code paths in debugexportstack are exercised:

  from edenscm.commands import debugstack
  with assertCovered(debugstack.debugexportstack):
    # Regular export.
    $ hg debugexportstack -r $B::$D | pprint
    [{'author': 'test', 'date': [0.0, 0], 'immutable': True, 'node': '983f771099bbf84b42d0058f027b47ede52f179a', 'relevantFiles': {'A': {'data': '1'}, 'B': None}, 'requested': False, 'text': 'A'},
     {'author': 'test',
      'date': [0.0, 0],
      'files': {'A': {'data': '2'}, 'B': {'data': '3', 'flags': 'x'}},
      'immutable': False,
      'node': '8b5b077308ecdd37270b7b94d98d64d27c170dfb',
      'parents': ['983f771099bbf84b42d0058f027b47ede52f179a'],
      'relevantFiles': {'C': None, 'Z': None},
      'requested': True,
      'text': 'B'},
     {'author': 'test',
      'date': [0.0, 0],
      'files': {'C': {'dataBase85': "b'`)v'"}, 'Z': {'data': 'B', 'flags': 'l'}},
      'immutable': False,
      'node': 'd2a2ca8387f2339934b6ce3fb17992433e06fdd4',
      'parents': ['8b5b077308ecdd37270b7b94d98d64d27c170dfb'],
      'relevantFiles': {'A': {'data': '2'}, 'D': None, 'E': None},
      'requested': True,
      'text': 'C'},
     {'author': 'test',
      'date': [0.0, 0],
      'files': {'A': None, 'D': {'copyFrom': 'A', 'data': '2'}, 'E': {'copyFrom': 'C', 'data': 'E'}},
      'immutable': False,
      'node': 'f5086e168b2741946a5118463a8be38273822529',
      'parents': ['d2a2ca8387f2339934b6ce3fb17992433e06fdd4'],
      'requested': True,
      'text': 'D'}]

    # Various kinds of limits.
    $ hg debugexportstack -r $B::$D --config experimental.exportstack-max-commit-count=2
    {"error": "too many commits"}
    [1]
    $ hg debugexportstack -r $B::$D --config experimental.exportstack-max-file-count=2
    {"error": "too many files"}
    [1]
    $ hg debugexportstack -r $B::$D --config experimental.exportstack-max-bytes=4B
    {"error": "too much data"}
    [1]

    # Export the working copy.
    $ hg go -q $D
    $ echo 3 > D
    $ echo X > X
    $ rm C
    $ hg addremove -q C X
    $ hg mv B B1
    $ hg debugexportstack -r 'wdir()' | pprint
    [{'author': 'test',
      'date': [0.0, 0],
      'immutable': False,
      'node': 'f5086e168b2741946a5118463a8be38273822529',
      'relevantFiles': {'B': {'data': '3', 'flags': 'x'}, 'B1': None, 'C': {'dataBase85': "b'`)v'"}, 'D': {'copyFrom': 'A', 'data': '2'}, 'X': None},
      'requested': False,
      'text': 'D'},
     {'author': 'test',
      'date': [0, 0],
      'files': {'B': None, 'B1': {'copyFrom': 'B', 'data': '3', 'flags': 'x'}, 'C': None, 'D': {'data': '3\n'}, 'X': {'data': 'X\n'}},
      'immutable': False,
      'node': 'ffffffffffffffffffffffffffffffffffffffff',
      'parents': ['f5086e168b2741946a5118463a8be38273822529'],
      'requested': True,
      'text': ''}]

Import stack:

  with assertCovered(
    debugstack.debugimportstack,
    debugstack._create_commits,
    debugstack._filectxfn
    debugstack._reset,
  ):
    # Simple linear stack
      $ newrepo
      $ hg debugimportstack << EOS | marks
      > [["commit", {"author": "test1", "date": [3600, 3600], "text": "A", "mark": ":1", "parents": [],
      >   "files": {"A": {"data": "A"}}}],
      >  ["commit", {"author": "test2", "date": [7200, 0], "text": "B", "mark": ":2", "parents": [":1"],
      >   "files": {"B": {"dataBase85": "LNN", "flags": "l"}}}],
      >  ["commit", {"author": "test3", "date": [7200, -3600], "text": "C", "mark": ":3", "parents": [":2"],
      >   "files": {"A": null, "C": {"data": "C1", "copyFrom": "A", "flags": "x"}}}],
      >  ["goto", {"mark": ":3"}]
      > ]
      > EOS
      {":1": "8e5dcdd5f19d443087e9916eecdac0505203e7c8", ":2": "c39ea291adacb1e3e0836ae80754a1bcff7bf9bc", ":3": "b32f0b24ea604d28def8eaf7730c4167ea79b35f"}

    # Check file contents and commit graph

      if hasfeature("execbit"):
        $ f -m C
        C: mode=755
      if hasfeature("symlink"):
        $ f B
        B -> B1

      $ cat C
      C1 (no-eol)

      $ hg log -Gr 'all()' -T '{desc} {author} {date|isodate}'
      @  C test3 1970-01-01 03:00 +0100
      │
      o  B test2 1970-01-01 02:00 +0000
      │
      o  A test1 1970-01-01 00:00 -0100

    # Fold

      $ hg debugimportstack << EOS | marks
      > [["commit", {"author": "test", "date": [0, 0], "text": "D", "mark": ":4",
      >   "parents": [], "predecessors": `marks :1 :2 :3`, "operation": "fold",
      >   "files": {"D": {"data": "D"}}}],
      >  ["goto", {"mark": ":4"}]]
      > EOS
      {":4": "058c1e1fb10a795a64351fb098ef497ea1b2ddbb"}

      $ ls
      D

      $ hg log -Gr 'all()' -T '{desc}'
      @  D

      $ hg debugmutation -r 'all()' | marks
       *  :4 fold by test at 1970-01-01T00:00:00 from:
          |-  :1
          |-  :2
          '-  :3

      $ hg hide 'desc(D)' -q

    # Split E -> [E1, E2, E3], and amend E -> E4, then reset

      $ hg debugimportstack << EOS | marks
      > [["commit", {"text": "E", "mark": ":5"}],
      >  ["commit", {"text": "E1", "mark": ":5a", "predecessors": [":5"]}],
      >  ["commit", {"text": "E2", "mark": ":5b", "predecessors": [":5"], "parents": [":5a"]}],
      >  ["commit", {"text": "E3", "mark": ":5c", "predecessors": [":5"], "parents": [":5b"], "operation": "split"}],
      >  ["commit", {"text": "E4", "mark": ":5d", "predecessors": [":5"], "operation": "amend"}],
      >  ["reset", {"mark": ":5c"}]]
      > EOS
      {":5": "163d5eee69569f6c170b946217ad981a726953ae", ":5a": "2a9f073f64d6ea3c1f8fd101515a7fb25cc1a20e", ":5b": "7eaade15648c4bd75f9884135ec311793ac5da01", ":5c": "9c69a6a007b9a6943f24635f36c1ad96b1feb8e2", ":5d": "4696154a532aa02b935321014b1ae9a61f94faea"}

    # Reset preserves "D" from the last "goto".

      $ ls

    # E should be hidden.

      $ hg log -Gr 'all()' -T '{desc}'
      o  E4
      
      @  E3
      │
      o  E2
      │
      o  E1

    # Check the mutation graph:
    # E1 (:5a) and E2 (:5b) should not have predecessor set.
    # E3 (:5c) should have "split into" information about E1 (:5a) and E2 (:5b).
    # E4 (:5c) should not have "split into" information.

      $ hg debugmutation -r 'all()' | marks
       *  :5a
      
       *  :5b
      
       *  :5c split by test at 1970-01-01T00:00:00 (split into this and: :5a, :5b) from:
          :5
      
       *  :5d amend by test at 1970-01-01T00:00:00 from:
          :5

    # Error cases
    $ hg debugimportstack << EOS
    > [["foo", {}]]
    > EOS
    {"error": "unsupported action: ['foo', {}]"}
    [1]
