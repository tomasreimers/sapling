# Copyright 2018 Facebook, Inc.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2 or any later version.

from __future__ import absolute_import

# Standard Library
import hashlib
import os
import socket
from subprocess import PIPE, Popen

from edenscm.mercurial import (
    commands,
    config,
    encoding,
    error,
    lock as lockmod,
    obsolete,
    pycompat,
    util,
    vfs as vfsmod,
)
from edenscm.mercurial.i18n import _

from . import commitcloudcommon, workspace


SERVICE = "commitcloud"
ACCOUNT = "commitcloud"


def _gethomevfs(ui, config_option_name):
    """
    Check config first.
    If config override is not given locate home dir
    Unix:
        returns the value of the 'HOME' environment variable
        if it is set and not equal to the empty string
    Windows:
        returns the value of the 'APPDATA' environment variable
        if it is set and not equal to the empty string
    """
    path = ui.config("commitcloud", config_option_name)
    if path and not os.path.isdir(path):
        raise commitcloudcommon.ConfigurationError(
            ui, _("invalid commitcloud.%s '%s'") % (config_option_name, path)
        )
    if path:
        return vfsmod.vfs(util.expandpath(path))

    if pycompat.iswindows:
        envvar = "APPDATA"
    else:
        envvar = "HOME"
    homedir = encoding.environ.get(envvar)
    if not homedir:
        raise commitcloudcommon.ConfigurationError(
            ui, _("$%s environment variable not found") % envvar
        )

    if not os.path.isdir(homedir):
        raise commitcloudcommon.ConfigurationError(
            ui, _("invalid homedir '%s'") % homedir
        )

    return vfsmod.vfs(homedir)


class TokenLocator(object):

    filename = ".commitcloudrc"

    def __init__(self, ui):
        self.ui = ui
        self.vfs = _gethomevfs(self.ui, "user_token_path")
        self.vfs.createmode = 0o600
        # using platform username
        self.secretname = (SERVICE + "_" + util.getuser()).upper()
        self.usesecretstool = self.ui.configbool("commitcloud", "use_secrets_tool")

    def _gettokenfromfile(self):
        """On platforms except macOS tokens are stored in a file"""
        if not self.vfs.exists(self.filename):
            if self.usesecretstool:
                # check if token has been backed up and recover it if possible
                try:
                    token = self._gettokenfromsecretstool()
                    if token:
                        self._settokentofile(token, isbackedup=True)
                    return token
                except Exception:
                    pass
            return None

        with self.vfs.open(self.filename, r"rb") as f:
            tokenconfig = config.config()
            tokenconfig.read(self.filename, f)
            token = tokenconfig.get("commitcloud", "user_token")
            if self.usesecretstool:
                isbackedup = tokenconfig.get("commitcloud", "backedup")
                if not isbackedup:
                    self._settokentofile(token)
            return token

    def _settokentofile(self, token, isbackedup=False):
        """On platforms except macOS tokens are stored in a file"""
        # backup token if optional backup is enabled
        if self.usesecretstool and not isbackedup:
            try:
                self._settokeninsecretstool(token)
                isbackedup = True
            except Exception:
                pass
        with self.vfs.open(self.filename, "w") as configfile:
            configfile.write(
                "[commitcloud]\nuser_token=%s\nbackedup=%s\n" % (token, isbackedup)
            )

    def _gettokenfromsecretstool(self):
        """Token stored in keychain as individual secret"""
        try:
            p = Popen(
                ["secrets_tool", "get", self.secretname],
                close_fds=util.closefds,
                stdout=PIPE,
                stderr=PIPE,
            )
            (stdoutdata, stderrdata) = p.communicate()
            rc = p.returncode
            if rc != 0:
                return None
            text = stdoutdata.strip()
            return text or None

        except OSError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)
        except ValueError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)

    def _settokeninsecretstool(self, token, update=False):
        """Token stored in keychain as individual secrets"""
        action = "update" if update else "create"
        try:
            p = Popen(
                [
                    "secrets_tool",
                    action,
                    "--read_contents_from_stdin",
                    self.secretname,
                    "Mercurial commitcloud token",
                ],
                close_fds=util.closefds,
                stdout=PIPE,
                stderr=PIPE,
                stdin=PIPE,
            )
            (stdoutdata, stderrdata) = p.communicate(token)
            rc = p.returncode

            if rc != 0:
                if action == "create":
                    # Try updating token instead
                    self._settokeninsecretstool(token, update=True)
                else:
                    raise commitcloudcommon.SubprocessError(self.ui, rc, stderrdata)

            else:
                self.ui.debug(
                    "access token is backup up in secrets tool in %s\n"
                    % self.secretname
                )

        except OSError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)
        except ValueError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)

    def _gettokenosx(self):
        """On macOS tokens are stored in keychain
           this function fetches token from keychain
        """
        try:
            args = [
                "security",
                "find-generic-password",
                "-g",
                "-s",
                SERVICE,
                "-a",
                ACCOUNT,
                "-w",
            ]
            p = Popen(
                args, stdin=None, close_fds=util.closefds, stdout=PIPE, stderr=PIPE
            )
            (stdoutdata, stderrdata) = p.communicate()
            rc = p.returncode
            if rc != 0:
                # security command is unable to show a prompt
                # from ssh sessions
                if rc == 36:
                    raise commitcloudcommon.KeychainAccessError(
                        self.ui,
                        "failed to access your keychain",
                        "please run `security unlock-keychain` "
                        "to prove your identity\n"
                        "the command `%s` exited with code %d" % (" ".join(args), rc),
                    )
                # if not found, not an error
                if rc == 44:
                    return None
                raise commitcloudcommon.SubprocessError(
                    self.ui,
                    rc,
                    "command: `%s`\nstderr: %s" % (" ".join(args), stderrdata),
                )
            text = stdoutdata.strip()
            if text:
                return text
            else:
                return None
        except OSError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)
        except ValueError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)

    def _settokenosx(self, token):
        """On macOS tokens are stored in keychain
           this function puts the token to keychain
        """
        try:
            args = [
                "security",
                "add-generic-password",
                "-a",
                ACCOUNT,
                "-s",
                SERVICE,
                "-w",
                token,
                "-U",
            ]
            p = Popen(
                args, stdin=None, close_fds=util.closefds, stdout=PIPE, stderr=PIPE
            )
            (stdoutdata, stderrdata) = p.communicate()
            rc = p.returncode
            if rc != 0:
                # security command is unable to show a prompt
                # from ssh sessions
                if rc == 36:
                    raise commitcloudcommon.KeychainAccessError(
                        self.ui,
                        "failed to access your keychain",
                        "please run `security unlock-keychain` "
                        "to prove your identity\n"
                        "the command `%s` exited with code %d" % (" ".join(args), rc),
                    )
                raise commitcloudcommon.SubprocessError(
                    self.ui,
                    rc,
                    "command: `%s`\nstderr: %s"
                    % (" ".join(args).replace(token, "<token>"), stderrdata),
                )
            self.ui.debug("new token is stored in keychain\n")
        except OSError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)
        except ValueError as e:
            raise commitcloudcommon.UnexpectedError(self.ui, e)

    @property
    def token(self):
        """Public API
            get token
                returns None if token is not found
                "not-required" is token is not needed
                it can throw only in case of unexpected error
        """
        if self.ui.configbool("commitcloud", "tls.notoken"):
            return "not-required"
        if self.ui.config("commitcloud", "user_token_path"):
            token = self._gettokenfromfile()
        elif pycompat.isdarwin:
            token = self._gettokenosx()
        else:
            token = self._gettokenfromfile()

        # Ensure token doesn't have any extraneous whitespace around it.
        if token is not None:
            token = token.strip()
        return token

    def settoken(self, token):
        """Public API
            set token
                it can throw only in case of unexpected error
        """
        # Ensure token doesn't have any extraneous whitespace around it.
        if token is not None:
            token = token.strip()

        if self.ui.config("commitcloud", "user_token_path"):
            self._settokentofile(token)
        elif pycompat.isdarwin:
            self._settokenosx(token)
        else:
            self._settokentofile(token)


class SubscriptionManager(object):

    dirname = ".commitcloud"
    joined = "joined"

    def __init__(self, repo):
        self.ui = repo.ui
        self.repo = repo
        self.workspacename = workspace.currentworkspace(repo)
        self.repo_name = getreponame(repo)
        self.repo_root = self.repo.path
        self.vfs = vfsmod.vfs(
            _gethomevfs(self.ui, "connected_subscribers_path").join(
                self.dirname, self.joined
            )
        )
        self.filename_unique = hashlib.sha256(
            "\0".join([self.repo_root, self.repo_name, self.workspacename])
        ).hexdigest()[:32]
        self.subscription_enabled = self.ui.configbool(
            "commitcloud", "subscription_enabled"
        )
        self.scm_daemon_tcp_port = self.ui.configint(
            "commitcloud", "scm_daemon_tcp_port"
        )

    def checksubscription(self):
        if not self.subscription_enabled:
            self.removesubscription()
            return
        if not self.vfs.exists(self.filename_unique):
            with self.vfs.open(self.filename_unique, "w") as configfile:
                configfile.write(
                    ("[commitcloud]\nworkspace=%s\nrepo_name=%s\nrepo_root=%s\n")
                    % (self.workspacename, self.repo_name, self.repo_root)
                )
                self._restart_service_subscriptions()
        else:
            self._test_service_is_running()

    def removesubscription(self):
        if self.vfs.exists(self.filename_unique):
            self.vfs.tryunlink(self.filename_unique)
            self._restart_service_subscriptions(warn_service_not_running=False)

    def _warn_service_not_running(self):
        self.ui.status(
            _(
                "scm daemon is not running and automatic synchronization may not work\n"
                "(run 'hg cloud sync' manually if your workspace is not synchronized)\n"
                "(please contact %s if this warning persists)\n"
            )
            % commitcloudcommon.getownerteam(self.ui),
            component="commitcloud",
        )

    def _test_service_is_running(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        if s.connect_ex(("127.0.0.1", self.scm_daemon_tcp_port)):
            self._warn_service_not_running()
        s.close()

    def _restart_service_subscriptions(self, warn_service_not_running=True):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            s.connect(("127.0.0.1", self.scm_daemon_tcp_port))
            s.send('["commitcloud::restart_subscriptions", {}]')
        except socket.error:
            if warn_service_not_running:
                self._warn_service_not_running()
        finally:
            s.close()


def getreponame(repo):
    """get the configured reponame for this repo"""
    reponame = repo.ui.config(
        "remotefilelog",
        "reponame",
        os.path.basename(repo.ui.config("paths", "default")),
    )
    if not reponame:
        raise commitcloudcommon.ConfigurationError(repo.ui, _("unknown repo"))
    return reponame


# Obsmarker syncing.
#
# To avoid lock interactions between transactions (which may add new obsmarkers)
# and sync (which wants to clear the obsmarkers), we use a two-stage process to
# sync obsmarkers.
#
# When a transaction completes, we append any new obsmarkers to the pending
# obsmarker file.  This is protected by the obsmarkers lock.
#
# When a sync operation starts, we transfer these obsmarkers to the syncing
# obsmarker file.  This transfer is also protected by the obsmarkers lock, but
# the transfer should be very quick as it's just moving a small amount of data.
#
# The sync process can upload the syncing obsmarkers at its leisure.  The
# syncing obsmarkers file is protected by the infinitepush backup lock.

_obsmarkerslockname = "commitcloudpendingobsmarkers.lock"
_obsmarkerslocktimeout = 2
_obsmarkerspending = "commitcloudpendingobsmarkers"
_obsmarkerssyncing = "commitcloudsyncingobsmarkers"


def addpendingobsmarkers(repo, markers):
    with lockmod.lock(repo.svfs, _obsmarkerslockname, timeout=_obsmarkerslocktimeout):
        with repo.svfs.open(_obsmarkerspending, "ab") as f:
            offset = f.tell()
            # offset == 0: new file - add the version header
            data = b"".join(
                obsolete.encodemarkers(markers, offset == 0, obsolete._fm1version)
            )
            f.write(data)


def getsyncingobsmarkers(repo):
    """Transfers any pending obsmarkers, and returns all syncing obsmarkers.

    The caller must hold the backup lock.
    """
    # Move any new obsmarkers from the pending file to the syncing file
    with lockmod.lock(repo.svfs, _obsmarkerslockname, timeout=_obsmarkerslocktimeout):
        if repo.svfs.exists(_obsmarkerspending):
            with repo.svfs.open(_obsmarkerspending) as f:
                _version, markers = obsolete._readmarkers(f.read())
            with repo.sharedvfs.open(_obsmarkerssyncing, "ab") as f:
                offset = f.tell()
                # offset == 0: new file - add the version header
                data = b"".join(
                    obsolete.encodemarkers(markers, offset == 0, obsolete._fm1version)
                )
                f.write(data)
            repo.svfs.unlink(_obsmarkerspending)

    # Load the syncing obsmarkers
    markers = []
    if repo.sharedvfs.exists(_obsmarkerssyncing):
        with repo.sharedvfs.open(_obsmarkerssyncing) as f:
            _version, markers = obsolete._readmarkers(f.read())
    return markers


def clearsyncingobsmarkers(repo):
    """Clears all syncing obsmarkers.  The caller must hold the backup lock."""
    repo.sharedvfs.unlink(_obsmarkerssyncing)


def getremotepath(repo, dest):
    # If dest is empty, pass in None to get the default path.
    path = repo.ui.paths.getpath(dest or None, default=("infinitepush", "default"))
    if not path:
        raise error.Abort(
            _("default repository not configured!"),
            hint=_("see 'hg help config.paths'"),
        )
    return path.pushloc or path.loc


def getcommandandoptions(command):
    cmd = commands.table[command][0]
    opts = dict(opt[1:3] for opt in commands.table[command][1])
    return cmd, opts
