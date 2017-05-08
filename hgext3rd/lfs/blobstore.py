from __future__ import absolute_import

import functools
import json

from mercurial import (
    error,
    url as urlmod,
    util,
)
from mercurial.i18n import _

from . import (
    util as lfsutil,
)

class StoreID(object):
    def __init__(self, oid, size):
        self.oid = oid
        self.size = size

class local(object):
    """Local blobstore for large file contents.

    This blobstore is used both as a cache and as a staging area for large blobs
    to be uploaded to the remote blobstore.
    """

    def __init__(self, repo):
        # deprecated config: lfs.blobstore
        storepath = repo.ui.config('lfs', 'blobstore', 'cache/localblobstore')
        fullpath = repo.vfs.join(storepath)
        self.vfs = lfsutil.lfsvfs(fullpath)

    def write(self, storeid, data):
        """Write blob to local blobstore."""
        with self.vfs(storeid.oid, 'wb', atomictemp=True) as fp:
            fp.write(data)

    def read(self, storeid):
        """Read blob from local blobstore."""
        return self.vfs.read(storeid.oid)

    def has(self, storeid):
        """Returns True if the local blobstore contains the requested blob,
        False otherwise."""
        return self.vfs.exists(storeid.oid)

class _gitlfsremote(object):

    def __init__(self, repo, url):
        ui = repo.ui
        self.ui = ui
        baseurl, authinfo = url.authinfo()
        self.baseurl = baseurl.rstrip('/')
        self.urlopener = urlmod.opener(ui, authinfo)

    def writebatch(self, storeids, fromstore, total=None):
        """Batch upload from local to remote blobstore."""
        self._batch(storeids, fromstore, 'upload', total=total)

    def readbatch(self, storeids, tostore, total=None):
        """Batch download from remote to local blostore."""
        self._batch(storeids, tostore, 'download', total=total)

    def _batch(self, storeids, localstore, action, total=None):
        if action not in ['upload', 'download']:
            # FIXME: we should not have that error raise too high
            raise UnavailableBatchOperationError(None, action)

        # Create the batch data for git-lfs.
        urlreq = util.urlreq
        objects = []
        storeidmap = {}
        for storeid in storeids:
            oid = storeid.oid[:40]  # Limitation in Dewey, hashes max 40 char
            size = storeid.size
            objects.append({
                'oid': oid,
                'size': size,
            })
            storeidmap[oid] = storeid

        requestdata = json.dumps({
            'objects': objects,
            'operation': action,
        })

        # Batch upload the blobs to git-lfs.
        if self.ui.verbose:
            self.ui.write(_('lfs: mapping blobs to %s URLs\n') % action)
        batchreq = urlreq.request('%s/objects/batch' % self.baseurl,
                                  data=requestdata)
        batchreq.add_header('Accept', 'application/vnd.git-lfs+json')
        batchreq.add_header('Content-Type', 'application/vnd.git-lfs+json')
        raw_response = self.urlopener.open(batchreq)
        response = json.loads(raw_response.read())

        topic = {'upload': _('lfs uploading'),
                 'download': _('lfs downloading')}[action]
        runningsize = 0
        if total is None:
            alttotal = functools.reduce(
                lambda acc, x: acc + long(x.get('size', 0)),
                response.get('objects'), 0)
            if alttotal > 0:
                total = alttotal
        if self.ui:
            self.ui.progress(topic, 0, total=total)
        for obj in response.get('objects'):
            oid = str(obj['oid'])
            try:
                # The action we're trying to perform should be available for the
                # current blob.
                if action not in obj.get('actions'):
                    raise UnavailableBatchOperationError(oid, action)

                size = long(obj.get('size'))
                href = str(obj['actions'][action].get('href'))
                headers = obj['actions'][action].get('header', {}).items()

                if self.ui:
                    self.ui.progress(topic, runningsize, total=total)

                if action == 'upload':
                    # If uploading blobs, read data from local blobstore.
                    filedata = localstore.read(storeidmap[oid])
                    request = urlreq.request(href, data=filedata)
                    request.get_method = lambda: 'PUT'
                else:
                    request = urlreq.request(href)

                for k, v in headers:
                    request.add_header(k, v)

                response = self.urlopener.open(request)

                if action == 'download':
                    # If downloading blobs, store downloaded data to local
                    # blobstore
                    localstore.write(storeidmap[oid], response.read())

                runningsize += size
            except util.urlerr.httperror:
                raise RequestFailedError(oid, action)
            except UnavailableBatchOperationError:
                if action == 'upload':
                    # The blob is already known by the remote blobstore.
                    continue
                else:
                    raise RequestFailedError(oid, action)

        self.ui.progress(topic, pos=None, total=total)
        if self.ui.verbose:
            self.ui.write(_('lfs: %s completed\n') % action)

    def __del__(self):
        # copied from mercurial/httppeer.py
        urlopener = getattr(self, 'urlopener', None)
        if urlopener:
            for h in urlopener.handlers:
                h.close()
                getattr(h, "close_all", lambda : None)()

class _dummyremote(object):
    """Dummy store storing blobs to temp directory."""

    def __init__(self, repo, url):
        fullpath = repo.vfs.join('lfs', url.path)
        self.vfs = lfsutil.lfsvfs(fullpath)

    def writebatch(self, storeids, fromstore, ui=None, total=None):
        for id in storeids:
            content = fromstore.read(id)
            with self.vfs(id.oid, 'wb', atomictemp=True) as fp:
                fp.write(content)

    def readbatch(self, storeids, tostore, ui=None, total=None):
        for id in storeids:
            content = self.vfs.read(id.oid)
            tostore.write(id, content)

class _nullremote(object):
    """Null store storing blobs to /dev/null."""

    def __init__(self, repo, url):
        pass

    def writebatch(self, storeids, fromstore, ui=None, total=None):
        pass

    def readbatch(self, storeids, tostore, ui=None, total=None):
        pass

class _promptremote(object):
    """Prompt user to set lfs.url when accessed."""

    def __init__(self, repo, url):
        pass

    def writebatch(self, storeids, fromstore, ui=None, total=None):
        self._prompt()

    def readbatch(self, storeids, tostore, ui=None, total=None):
        self._prompt()

    def _prompt(self):
        raise error.Abort(_('lfs.url needs to be configured'))

_storemap = {
    'https': _gitlfsremote,
    'http': _gitlfsremote,
    'file': _dummyremote,
    'null': _nullremote,
    None: _promptremote,
}

def remote(repo):
    """remotestore factory. return a store in _storemap depending on config"""
    defaulturl = ''

    # convert deprecated configs to the new url. TODO: remove this if other
    # places are migrated to the new url config.
    # deprecated config: lfs.remotestore
    deprecatedstore = repo.ui.config('lfs', 'remotestore')
    if deprecatedstore == 'dummy':
        # deprecated config: lfs.remotepath
        defaulturl = 'file://' + repo.ui.config('lfs', 'remotepath')
    elif deprecatedstore == 'git-lfs':
        # deprecated config: lfs.remoteurl
        defaulturl = repo.ui.config('lfs', 'remoteurl')
    elif deprecatedstore == 'null':
        defaulturl = 'null://'

    url = util.url(repo.ui.config('lfs', 'url', defaulturl))
    scheme = url.scheme
    if scheme not in _storemap:
        raise error.Abort(_('lfs: unknown url scheme: %s') % scheme)
    return _storemap[scheme](repo, url)

class RequestFailedError(error.RevlogError):
    def __init__(self, oid, action):
        message = _('the requested file could be %sed: %s') % (action, oid)
        super(RequestFailedError, self).__init__(message)

class UnavailableBatchOperationError(error.RevlogError):
    def __init__(self, oid, action):
        self.oid = oid
        self.action = action

        message = (_('unknown batch operation "%s" for blob "%s"')
                   % (self.action, self.oid or 'unknown'))
        super(UnavailableBatchOperationError, self).__init__(message)
