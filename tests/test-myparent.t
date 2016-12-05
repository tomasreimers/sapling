Setup

  $ PYTHONPATH=$TESTDIR/..:$PYTHONPATH
  $ export PYTHONPATH
  $ cat >> $HGRCPATH << EOF
  > [extensions]
  > myparent=$TESTDIR/../hgext3rd/myparent.py
  > EOF
  $ hg init repo
  $ cd repo
  $ touch foo
  $ cat >> ../commitmessage << EOF
  > [prefix] My title
  > 
  > Summary: Very good summary of my commit.
  > 
  > Test Plan: cat foo
  > 
  > Reviewers: #sourcecontrol, rmcelroy
  > 
  > Subscribers: rmcelroy, mjpieters
  > 
  > Differential Revision: https://phabricator.fb.com/D42
  > 
  > Tasks: 1337
  > EOF
  $ hg commit -qAl ../commitmessage
  $ touch bar
  $ hg commit -qAm 'Differential Revision: https://phabricator.fb.com/D2'

All template keywords work if the current author matches the other of the
previous commit.

  $ hg log -T '{myparentdiff}\n' -r .
  D42
  $ hg log -T '{myparentreviewers}\n' -r .
  #sourcecontrol, rmcelroy
  $ hg log -T '{myparentsubscribers}\n' -r .
  rmcelroy, mjpieters
  $ hg log -T '{myparenttasks}\n' -r .
  1337
  $ hg log -T '{myparenttitleprefix}\n' -r .
  [prefix]

If the authors do not match the keywords will be empty.

  $ hg commit -q --amend --user hacker2
  $ hg log -T '{myparentdiff}' -r .
  $ hg log -T '{myparentreviewers}' -r .
  $ hg log -T '{myparentsubscribers}' -r .
  $ hg log -T '{myparenttasks}' -r .
  $ hg log -T '{myparenttitleprefix}' -r .

