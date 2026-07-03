! Regression (fpm cmd_new SIGBUS on `call run('cd '//name)`): the optional-
! parameter and character-length masks are cached in maps keyed by bare
! procedure name. fpm defines several `run` subroutines; the last-collected
! one (m_other::run below, 3 non-optional args) clobbered the bare "run"
! entry. A call that resolved to m_fs::run then read the wrong optional/
! char-len masks, so the lowering dropped the omitted optional-null pointers
! AND the hidden character-length of `cmd` — the callee read len(cmd) and
! present(redirect) from uninitialised registers (crash / wrong answer).
!
! With the qualified-key fix the call reads m_fs::run's own masks: cmd gets
! its hidden length, and every omitted optional is passed as a null.
module m_fs
  implicit none
contains
  subroutine run(cmd, echo, exitstat, verbose, redirect)
     character(len=*), intent(in) :: cmd
     logical, intent(in), optional :: echo
     integer, intent(out), optional :: exitstat
     logical, intent(in), optional :: verbose
     character(*), intent(in), optional :: redirect
     integer :: n
     n = len(cmd)
     write(*, '(a,i0,a,l1,a,l1,a,l1,a,l1)') 'len=', n, &
        ' pe=', present(echo), ' px=', present(exitstat), &
        ' pv=', present(verbose), ' pr=', present(redirect)
     if (n /= 11) error stop 1
     if (present(echo) .or. present(exitstat) .or. &
         present(verbose) .or. present(redirect)) error stop 2
  end subroutine

  subroutine driver(name)
     character(len=*), intent(in) :: name
     call run('cd '//name)   ! all four optionals omitted
  end subroutine
end module

module m_other
  implicit none
  type :: ctx_t
     integer :: x = 0
  end type
contains
  ! Same bare name `run`, defined AFTER m_fs, mirroring fpm_installer::run:
  ! a character(len=*) arg at position 1 (not 0) and no optionals. Its
  ! char-length mask [F,T,F] clobbers m_fs::run's [T,F,F,F,T] under the bare
  ! key, so without the fix the `cd` call drops cmd's hidden length.
  subroutine run(self, command, error)
     type(ctx_t), intent(inout) :: self
     character(len=*), intent(in) :: command
     integer, intent(out) :: error
     self%x = self%x + len(command)
     error = 0
  end subroutine
end module

program p
  use m_fs, only: driver
  implicit none
  call driver('projname')   ! 'cd '//'projname' -> len 11
  print *, 'DONE'
end program p
! CHECK: len=11 pe=F px=F pv=F pr=F
! CHECK: DONE
