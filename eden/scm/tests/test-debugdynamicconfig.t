#chg-compatible

  $ configure modern

  $ setconfig configs.loaddynamicconfig=True
  $ export HG_TEST_DYNAMICCONFIG="$TESTTMP/test_hgrc"
  $ cat > test_hgrc <<EOF
  > [section]
  > key=value
  > EOF

  $ hg init client
  $ cd client

Verify it can be manually generated

  $ hg debugdynamicconfig
  $ cat .hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [section]
  key=value
  
  $ hg config section.key
  value

Verify it can be automatically synchronously generated

  $ rm .hg/hgrc.dynamic
  $ hg config section.key
  value
  $ cat .hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [section]
  key=value
  
Verify it can be automatically asynchronously regenerated

  $ cat > $TESTTMP/test_hgrc <<EOF
  > [section]
  > key=value
  > [section2]
  > key2=value2
  > EOF
  $ hg config section2.key2 --config configs.generationtime=30 # No regen, because too soon
  [1]
  $ sleep 1
  $ hg status --config configs.generationtime=1 # Regen, because lower time limit
  $ sleep 0.5 # Time for background process to complete
  $ hg config section2.key2
  value2
  $ cat .hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [section]
  key=value
  
  [section2]
  key2=value2
  
Verify mtime is updated even if no change is made
  $ python -c "import stat, os; print(os.stat(os.path.join('.hg', 'hgrc.dynamic'))[stat.ST_MTIME])" > $TESTTMP/mtime1
  $ hg status --config configs.generationtime=60 # No regen, because high time limit
  $ python -c "import stat, os; print(os.stat(os.path.join('.hg', 'hgrc.dynamic'))[stat.ST_MTIME])" > $TESTTMP/mtime2
  $ diff -q $TESTTMP/mtime1 $TESTTMP/mtime2 >/dev/null 2>/dev/null

  $ sleep 1
  $ hg status --config configs.generationtime=1 # Regen, because low time limit
  $ python -c "import stat, os; print(os.stat(os.path.join('.hg', 'hgrc.dynamic'))[stat.ST_MTIME])" > $TESTTMP/mtime3
  $ diff -q $TESTTMP/mtime2 $TESTTMP/mtime3 >/dev/null 2>/dev/null
  [1]

Validate dynamic config
  $ cat > $TESTTMP/input_hgrc <<EOF
  > [section]
  > key=valueX
  > EOF
  $ echo "%include $TESTTMP/input_hgrc" >> .hg/hgrc
  $ hg status --config configs.validatedynamicconfig=True --config configs.mismatchwarn=True --config configs.testdynamicconfigsubset=input_hgrc
  Config mismatch: section2.key2 has 'value2' (dynamic) vs 'None' (file)
  Config mismatch: section.key has 'value' (dynamic) vs 'valueX' (file)
  Config mismatch: section2.key2 has 'value2' (dynamic) vs 'None' (file)
  Config mismatch: section.key has 'value' (dynamic) vs 'valueX' (file)

Verify we generate and load from a shared repo

  $ cd ..
  $ enable share
  $ hg init shared
  $ hg share shared shared_copy
  updating working directory
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  $ cd shared_copy
  $ hg debugdynamicconfig
  $ test -f .hg/hgrc.dynamic
  [1]
  $ cat ../shared/.hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [section]
  key=value
  
  [section2]
  key2=value2
  
  $ hg config section.key
  value

Verify we regenerate configs if the Mercurial version differs
  $ cat > ../shared/.hg/hgrc.dynamic <<EOF
  > # version=1
  > [section3]
  > key3=value3
  > EOF
  $ hg config section3.key3
  [1]
  $ hg config section.key
  value
  $ cat ../shared/.hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [section]
  key=value
  
  [section2]
  key2=value2
  
Verify we don't regenerate configs if the Mercurial version hasn't changed
  $ cat >> ../shared/.hg/hgrc.dynamic <<EOF
  > [section3]
  > key3=value3
  > EOF
  $ hg config section3.key3
  value3

Verify we load and verify dynamicconfigs during clone
  $ newserver server
  $ cd $TESTTMP
  $ export HG_TEST_DYNAMICCONFIG="$TESTTMP/test_hgrc"
  $ cat > test_hgrc <<EOF
  > [hooks]
  > pretxnclose = printf "Hook ran!\n"
  > EOF
  $ cat > good_hgrc <<EOF
  > [hooks]
  > pretxnclose = printf "Hook ran!\n"
  > [foo]
  > bar=True
  > EOF
  $ hg clone ssh://user@dummy/server client2 --configfile $TESTTMP/good_hgrc --config configs.testdynamicconfigsubset=good_hgrc --config configs.validatedynamicconfig=True --config configs.mismatchwarn=True
  Config mismatch: foo.bar has 'None' (dynamic) vs 'True' (file)
  no changes found
  Hook ran!
  updating to branch default
  0 files updated, 0 files merged, 0 files removed, 0 files unresolved
  Hook ran!
  Hook ran!
  $ cat client2/.hg/hgrc.dynamic
  # version=4.4.2* (glob)
  # Generated by `hg debugdynamicconfig` - DO NOT MODIFY
  [hooks]
  pretxnclose=printf "Hook ran!\n"
  
