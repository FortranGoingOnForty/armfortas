! Regression (fpm cmd_new wrote its manifest to stdout, never to fpm.toml): a
! plain `call sub(...)` from inside an internal subprogram must bind to the
! procedure the host module actually imported via `use ONLY`, not to the first
! same-named callable found in source order across all scopes.
!
! An internal subprogram's link name (afs_internal_...) does not map back to a
! sema scope, so callee_scope_id_for_lookup returned None and resolution fell
! through to a global scan that picked the first same-named procedure. fpm's
! create_verified_basic_manifest (internal to cmd_new) `use`s only
! fpm_filesystem's `fileopen`, but M_CLI2 also exports a `fileopen` defined
! earlier; the scan bound the call to M_CLI2's function fileopen. Its signature
! differs, so `filename` arrived blank -> fileopen's blank-name branch set the
! unit to stdout, and the manifest write went to the terminal instead of disk.
! The fix falls back to current_proc_scope(), which resolves the call through
! the caller's own USE/host association. Without it the program prints
! WRONG-MODULE and no WROTE line, so the CHECK below fails.

! Module A: a same-named `wr` defined FIRST, NOT imported by the caller.
module m_a
  implicit none
  private
  public :: wr
contains
  integer function wr(path, mode) result(u)
     character(len=*), intent(in) :: path, mode
     u = 0
     print '(a)', 'WRONG-MODULE '//trim(path)//' '//trim(mode)
  end function wr
end module

! Module B: the `wr` subroutine the caller actually imports.
module m_b
  implicit none
  private
  public :: wr
contains
  subroutine wr(filename, payload)
     character(len=*), intent(in) :: filename, payload
     integer :: u, ios
     open(file=filename, newunit=u, form='formatted', access='sequential', &
          action='write', position='rewind', status='replace', iostat=ios)
     if (ios /= 0) then
        print '(a)', 'OPEN-FAIL'
        return
     end if
     write(u,'(a)') payload
     close(u)
     print '(a)', 'WROTE='//trim(payload)
  end subroutine wr
end module

module m_c
  use m_b, only : wr
  implicit none
  private
  public :: run
contains
  subroutine run(fname)
     character(len=*), intent(in) :: fname
     call emit(fname)     ! calls internal subprogram below
  contains
     subroutine emit(filename)   ! internal subprogram, like create_verified_basic_manifest
        character(len=*), intent(in) :: filename
        call wr(filename, 'hello-from-manifest')
     end subroutine emit
  end subroutine run
end module

program p
  use m_c, only : run
  implicit none
  call run('/tmp/x12_use_only_call_out.txt')
  print '(a)', 'DONE'
end program p
! CHECK: WROTE=hello-from-manifest
! CHECK: DONE
