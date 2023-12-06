# ============================================================================ #
#
#
#
# ---------------------------------------------------------------------------- #

SRC_DIR := $(call getMakefileDir)


# ---------------------------------------------------------------------------- #

# _BUILT_SRCS := buildenv.nix get-env.sh
# _BUILT      := $(patsubst %,$(SRC_DIR)/%.gen.hh,$(_BUILT_SRCS))
# _BUILT_SRCS =

BUILT_SRCS              +=
envbuilder_SRCS         +=
flox-env-builder_SRCS   += $(wildcard $(SRC_DIR)/*.cc)
flox-env-builder_LDLIBS += -lpkgdb
libenvbuilder_LDLIBS    += -lsqlite3


# ---------------------------------------------------------------------------- #
#
#
#
# ============================================================================ #
