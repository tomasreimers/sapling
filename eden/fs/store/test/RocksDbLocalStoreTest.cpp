/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include "eden/fs/store/RocksDbLocalStore.h"
#include "eden/fs/store/test/LocalStoreTest.h"
#include "eden/fs/telemetry/NullStructuredLogger.h"

namespace {

using namespace facebook::eden;

LocalStoreImplResult makeRocksDbLocalStore(FaultInjector* faultInjector) {
  auto tempDir = makeTempDir();
  auto store = std::make_unique<RocksDbLocalStore>(
      AbsolutePathPiece{tempDir.path().string()},
      std::make_shared<NullStructuredLogger>(),
      faultInjector);
  return {std::move(tempDir), std::move(store)};
}

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
INSTANTIATE_TEST_CASE_P(
    RocksDB,
    LocalStoreTest,
    ::testing::Values(makeRocksDbLocalStore));
#pragma clang diagnostic pop

} // namespace
