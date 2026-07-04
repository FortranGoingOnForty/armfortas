! Regression (fpm build SIGSEGV parsing fpm.toml): a generic call must not be
! hijacked by an INTERNAL subprogram of an unrelated procedure that happens to
! share the generic's name.
!
! The internal-subprogram map is unit-wide (every internal sub in the file,
! keyed by bare name). resolve_subroutine_call_name shortcut on
! `internal_funcs.contains_key(key)` alone, so a call to the generic `resize`
! (imported by m_context from m_token) was diverted to the internal-sub
! resolution path because m_parser's `parse_hdr` also has an internal
! subprogram named `resize` -- for a DIFFERENT, larger derived type. That path
! global-scanned to the wrong `resize`, which walked the toml_token array with
! toml_key strides and dereferenced token integers as a string pointer.
!
! In fpm this was tomlf_de_context%push_back's `resize(self%token)` binding to
! tomlf_de_parser::parse_table_header's internal `resize` (for toml_key). The
! fix takes the internal-sub branch only when the caller's own scope resolves
! the name to a host-associated concrete procedure. Without it this program
! SIGSEGVs (rc=139) while growing the token array.

module m_token
  implicit none
  private
  public :: tok_t, resize
  type :: tok_t
     integer :: kind = 0, first = 0, last = 0, chunk = 0
  end type
  interface resize
     module procedure resize_tok
  end interface
contains
  pure subroutine resize_tok(var, n)
     type(tok_t), allocatable, intent(inout) :: var(:)
     integer, intent(in), optional :: n
     type(tok_t), allocatable :: tmp(:)
     integer :: m, i
     m = 8
     if (allocated(var)) m = 2 * size(var)   ! grow, don't pin to 8
     if (present(n)) m = n
     if (allocated(var)) then
        call move_alloc(var, tmp)
        allocate(var(m))
        do i = 1, min(size(tmp), m); var(i) = tmp(i); end do
     else
        allocate(var(m))
     end if
  end subroutine
end module

! parser-like module whose `parse_hdr` has an INTERNAL subprogram named `resize`
! for a bigger derived type with an allocatable character component.
module m_parser
  implicit none
  private
  public :: key_t, parse_hdr
  type :: key_t
     character(len=:), allocatable :: k
     integer :: origin = 0
  end type
contains
  subroutine parse_hdr(stack, cnt)
     type(key_t), allocatable, intent(inout) :: stack(:)
     integer, intent(in) :: cnt
     integer :: i
     do i = 1, cnt
        if (.not.allocated(stack)) call resize(stack)
        if (i > size(stack)) call resize(stack)
     end do
  contains
     subroutine resize(s, n)   ! internal subprogram named `resize`, key_t
        type(key_t), allocatable, intent(inout) :: s(:)
        integer, intent(in), optional :: n
        type(key_t), allocatable :: tmp(:)
        integer :: m, j
        m = 8
        if (allocated(s)) m = 2 * size(s)
        if (present(n)) m = n
        if (allocated(s)) then
           call move_alloc(s, tmp)
           allocate(s(m))
           do j = 1, min(size(tmp), m); s(j) = tmp(j); end do
        else
           allocate(s(m))
        end if
     end subroutine
  end subroutine
end module

module m_context
  use m_token, only : tok_t, resize   ! the generic resize for tok_t
  implicit none
  private
  public :: ctx_t
  type :: ctx_t
     type(tok_t), allocatable :: token(:)
     integer :: top = 0
   contains
     procedure :: push
  end type
contains
  subroutine push(self, t)
     class(ctx_t), intent(inout) :: self
     type(tok_t), intent(in) :: t
     if (.not.allocated(self%token)) call resize(self%token)
     if (self%top >= size(self%token)) call resize(self%token)
     self%top = self%top + 1
     self%token(self%top) = t
  end subroutine
end module

program p
  use m_context, only : ctx_t
  use m_token, only : tok_t
  implicit none
  type(ctx_t) :: c
  type(tok_t) :: t
  integer :: i
  do i = 1, 20
     t%kind = i; t%first = i*2; t%last = i*3; t%chunk = i*4
     call c%push(t)
  end do
  write(*,'(a,i0,1x,i0,1x,i0)') 'top=', c%top, c%token(20)%kind, c%token(20)%last
  print '(a)', 'DONE'
end program
! CHECK: top=20 20 60
! CHECK: DONE
