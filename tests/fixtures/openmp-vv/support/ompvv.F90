! ARMFORTAS host-only compatibility header for pinned OpenMP V&V sources.
! The upstream umbrella header also defines offload probes outside this
! tranche. Keep this adapter small and preserve imported test sources intact.

#define OMPVV_NUM_THREADS_HOST 4
#define OMPVV_TEST_VERBOSE(condition) CALL ompvv_test(condition)
#define OMPVV_WARNING_IF(condition, message) CONTINUE
#define OMPVV_REPORT_AND_RETURN() CALL ompvv_finish()

MODULE ompvv_lib
  IMPLICIT NONE
  INTEGER, PRIVATE :: ompvv_errors = 0
CONTAINS
  SUBROUTINE ompvv_test(failed)
    LOGICAL, INTENT(IN) :: failed
    IF (failed) ompvv_errors = ompvv_errors + 1
  END SUBROUTINE ompvv_test

  SUBROUTINE ompvv_finish()
    IF (ompvv_errors /= 0) ERROR STOP 1
  END SUBROUTINE ompvv_finish
END MODULE ompvv_lib
