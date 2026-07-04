! Regression (fpm build SIGBUS invoking the compiler): a keyword-argument call
! must be reordered against the RESOLVED callee's parameter list, not against a
! same-named procedure in another module with a different parameter order.
!
! reorder_args_by_keyword_slots looked the callee's arg_order up by bare name,
! which for an ambiguous name returns the last-defined match. fpm defines both
! fpm_filesystem::run(cmd, echo, exitstat, verbose, redirect) and
! fpm_installer::run(self, command, error). compile_fortran's
! `call run(command, echo=..., verbose=..., redirect=log_file, exitstat=stat)`
! was reordered against the installer's (self, command, error), so echo and
! exitstat fell to null and the hidden character lengths landed in the wrong
! slots -- `run` read `redirect`'s length from garbage and faulted. The fix
! reorders against the resolved target's own arg_order. Without it this program
! SIGBUSes (rc=138) building the redirect string from a garbage length.

module m_fs
  implicit none
  private
  public :: run
contains
  subroutine run(cmd, echo, exitstat, verbose, redirect)
     character(*), intent(in) :: cmd
     logical, intent(in), optional :: echo
     integer, intent(out), optional :: exitstat
     logical, intent(in), optional :: verbose
     character(*), intent(in), optional :: redirect
     character(:), allocatable :: rstr
     if (present(redirect)) then
        if (redirect /= '') then
           rstr = '>'//redirect//' 2>&1'
        else
           rstr = ''
        end if
     else
        rstr = '(none)'
     end if
     write(*,'(a)') 'rstr=['//rstr//']'
     if (present(exitstat)) exitstat = 0
  end subroutine
end module

! a competing `run` with different parameters, defined LATER (last-defined wins
! the bare-name arg_order lookup).
module m_inst
  implicit none
  private
  public :: run
contains
  subroutine run(self, command, error)
     integer, intent(in) :: self
     character(*), intent(in) :: command
     integer, intent(out) :: error
     error = 0
     write(*,'(a)') 'INSTALLER: '//trim(command)
  end subroutine
end module

module m_comp
  use m_fs, only : run
  implicit none
  private
  public :: compiler_t, compile
  type :: compiler_t
     logical :: echo = .true.
     logical :: verbose = .false.
  end type
contains
  subroutine compile(self, input, log_file)
     class(compiler_t), intent(in) :: self
     character(*), intent(in) :: input, log_file
     character(:), allocatable :: command
     integer :: stat
     command = 'fc -c '//input
     call run(command, echo=self%echo, verbose=self%verbose, redirect=log_file, exitstat=stat)
     write(*,'(a,i0)') 'stat=', stat
  end subroutine
end module

program p
  use m_comp, only : compiler_t, compile
  implicit none
  type(compiler_t) :: c
  call compile(c, 'demo.f90', 'build.log')
  print '(a)', 'DONE'
end program
! CHECK: rstr=[>build.log 2>&1]
! CHECK: stat=0
! CHECK: DONE
